mod cache;
mod cargo;
mod command;
mod project;
mod source;

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{CommandFactory, Parser};
use directories::ProjectDirs;
use serde::Serialize;
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "paths",
    version,
    about = "Print local source paths for dependencies and GitHub repositories"
)]
struct Args {
    /// Dependency names, name@exact-version requests, or HTTPS GitHub repository URLs
    #[arg(required = true, value_name = "REQUEST")]
    dependencies: Vec<String>,
    /// Full commit hash, branch, tag, or HEAD (requires exactly one repository URL)
    #[arg(long = "ref", value_name = "REF", value_parser = source::parse_reference)]
    reference: Option<String>,
    /// Exact workspace name or path relative to the project root
    #[arg(short = 'F', long)]
    filter: Option<String>,
    /// Override package-manager detection
    #[arg(long, value_enum)]
    package_manager: Option<project::Manager>,
    /// Emit one JSON result per request, including failures
    #[arg(long)]
    json: bool,
    /// Override the checkout cache directory (npm/pnpm and GitHub URLs)
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

impl Args {
    fn validate(&self) -> Result<()> {
        if self.reference.is_some() {
            ensure!(
                self.dependencies.len() == 1
                    && source::repository_request(&self.dependencies[0])?.is_some(),
                "--ref requires exactly one HTTPS GitHub repository URL"
            );
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Resolution {
    Package(Resolved),
    Repository(source::Source),
}

#[derive(Serialize)]
struct Resolved {
    name: String,
    version: String,
    #[serde(flatten)]
    source: ResolvedSource,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ResolvedSource {
    Git(source::Source),
    Cargo { path: PathBuf, method: &'static str },
}

struct NodePackage {
    name: String,
    version: String,
    manifest: Value,
}

fn node_package(
    spec: &str,
    args: &Args,
    root: &std::path::Path,
    manager: project::Manager,
    projects: &mut Option<Vec<project::Project>>,
    installed_only: bool,
) -> Result<Option<NodePackage>> {
    let (requested_name, requested_version) = project::parse_spec(spec)?;
    if let Some(version) = requested_version
        && !installed_only
    {
        return Ok(Some(NodePackage {
            name: requested_name.to_owned(),
            version: version.to_owned(),
            manifest: Value::Null,
        }));
    }
    if projects.is_none() {
        *projects = Some(project::load_projects(root, manager)?);
    }
    let Some(dependency) = project::select_dependency(
        projects.as_deref().unwrap_or_default(),
        root,
        args.filter.as_deref(),
        requested_name,
        requested_version,
    )?
    else {
        return Ok(None);
    };
    let manifest = project::read_json(&dependency.path.join("package.json"))?;
    let name = manifest["name"]
        .as_str()
        .context("installed package has no name")?
        .to_owned();
    let version = manifest["version"]
        .as_str()
        .context("installed package has no version")?
        .to_owned();
    project::parse_spec(&format!("{name}@{version}"))?;
    Ok(Some(NodePackage {
        name,
        version,
        manifest,
    }))
}

fn source_cache<'a>(cache: &'a mut Option<PathBuf>, args: &Args) -> Result<&'a std::path::Path> {
    if cache.is_none() {
        let path = args
            .cache_dir
            .clone()
            .or_else(|| {
                ProjectDirs::from("", "", "paths-cli").map(|dirs| dirs.cache_dir().to_path_buf())
            })
            .context("cannot determine cache directory; use --cache-dir")?;
        fs::create_dir_all(&path)?;
        *cache = Some(dunce::canonicalize(path)?);
    }
    cache.as_deref().context("missing source cache")
}

fn materialize_node(
    package: NodePackage,
    args: &Args,
    root: &std::path::Path,
    manager: project::Manager,
    cache: &mut Option<PathBuf>,
) -> Result<Resolved> {
    let cache = source_cache(cache, args)?;
    let NodePackage {
        name,
        version,
        manifest,
    } = package;
    let mut metadata = match project::metadata(root, manager, &name, &version) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("warning: {name}@{version}: registry metadata unavailable: {error:#}");
            Value::Null
        }
    };
    let repository = source::normalize_repository(&manifest["repository"])
        .or_else(|| source::normalize_repository(&metadata["repository"]))
        .context("package has no usable Git repository URL")?;
    if metadata.is_null() {
        metadata = manifest;
    }
    let source = source::materialize(cache, &repository, &version, &metadata)?;
    if matches!(
        source.method,
        source::Method::PublicationTime | source::Method::DefaultBranch
    ) {
        eprintln!(
            "warning: {name}@{version}: {:?} fallback at {}; source may differ from the published package",
            source.method, source.commit
        );
    }
    Ok(Resolved {
        name,
        version,
        source: ResolvedSource::Git(source),
    })
}

#[derive(Default)]
struct Resolver {
    projects: Option<Vec<project::Project>>,
    cargo: cargo::Resolver,
    cache: Option<PathBuf>,
}

impl Resolver {
    fn resolve(
        &mut self,
        spec: &str,
        args: &Args,
        root: &std::path::Path,
        manager: Option<project::Manager>,
    ) -> Result<Resolved> {
        let (name, version) = project::parse_spec(spec)?;
        if manager == Some(project::Manager::Cargo) {
            return self
                .cargo
                .resolve(root, args.filter.as_deref(), name, version)?
                .with_context(|| format!("resolved Cargo dependency `{spec}` not found"));
        }
        if let Some(manager) = manager {
            let package = node_package(spec, args, root, manager, &mut self.projects, false)?
                .with_context(|| format!("installed dependency `{spec}` not found"))?;
            return materialize_node(package, args, root, manager, &mut self.cache);
        }
        let node = project::discover_node(root, None).and_then(|(root, manager)| {
            node_package(spec, args, &root, manager, &mut self.projects, true)
                .map(|package| package.map(|package| (root, manager, package)))
        });
        let cargo = self
            .cargo
            .resolve(root, args.filter.as_deref(), name, version);
        match (node, cargo) {
            (Ok(Some(_)), Ok(Some(_))) => {
                bail!(
                    "dependency `{spec}` found in both Node and Cargo projects; use --package-manager npm|pnpm|cargo"
                )
            }
            (Ok(Some((root, manager, package))), cargo) => {
                if let Err(error) = cargo {
                    eprintln!("warning: cannot inspect Cargo dependencies: {error:#}");
                }
                materialize_node(package, args, &root, manager, &mut self.cache)
            }
            (node, Ok(Some(resolved))) => {
                if let Err(error) = node {
                    eprintln!("warning: cannot inspect Node dependencies: {error:#}");
                }
                Ok(resolved)
            }
            (Ok(None), Ok(None)) => {
                bail!("dependency `{spec}` not found in Node or Cargo projects")
            }
            (node, cargo) => {
                let diagnostic = |error: Option<anyhow::Error>| {
                    error.map_or_else(|| "not found".to_owned(), |error| format!("{error:#}"))
                };
                bail!(
                    "cannot resolve `{spec}`; Node: {}; Cargo: {}",
                    diagnostic(node.err()),
                    diagnostic(cargo.err())
                )
            }
        }
    }
}

fn run(args: &Args) -> Result<bool> {
    let cwd = dunce::canonicalize(std::env::current_dir()?)?;
    let mut project = None;
    let mut usage = cache::Usage::default();
    let mut state = Resolver::default();
    let mut failed = false;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for spec in &args.dependencies {
        let result = (|| {
            if let Some(repository) = source::repository_request(spec)? {
                return source::materialize_repository(
                    source_cache(&mut state.cache, args)?,
                    &repository,
                    args.reference.as_deref(),
                )
                .map(Resolution::Repository);
            }
            let (root, manager) = match &project {
                Some(project) => project,
                None => project.insert(project::discover(&cwd, args.package_manager)?),
            };
            state
                .resolve(spec, args, root, *manager)
                .map(Resolution::Package)
        })();
        match result {
            Ok(resolved) => {
                let path = match &resolved {
                    Resolution::Repository(source)
                    | Resolution::Package(Resolved {
                        source: ResolvedSource::Git(source),
                        ..
                    }) => {
                        usage.record(source);
                        &source.path
                    }
                    Resolution::Package(Resolved {
                        source: ResolvedSource::Cargo { path, .. },
                        ..
                    }) => path,
                };
                if args.json {
                    writeln!(
                        output,
                        "{}",
                        serde_json::json!({"request": spec, "result": resolved})
                    )?;
                } else {
                    writeln!(output, "{}", path.display())?;
                }
            }
            Err(error) => {
                failed = true;
                eprintln!("error: {spec}: {error:#}");
                if args.json {
                    writeln!(
                        output,
                        "{}",
                        serde_json::json!({"request": spec, "error": format!("{error:#}")})
                    )?;
                }
            }
        }
    }
    if let Some(cache) = state.cache {
        usage.cleanup(&cache, cache::MAX_BYTES);
    }
    Ok(failed)
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Err(error) = args.validate() {
        Args::command()
            .error(clap::error::ErrorKind::InvalidValue, error.to_string())
            .exit();
    }
    match run(&args) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
