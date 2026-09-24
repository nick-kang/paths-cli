mod command;
mod project;
mod source;

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::Parser;
use directories::ProjectDirs;
use serde::Serialize;
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "paths",
    version,
    about = "Print cached Git source paths for npm and pnpm dependencies"
)]
struct Args {
    /// Installed dependency names or name@exact-version requests
    #[arg(required = true)]
    dependencies: Vec<String>,
    /// Exact workspace name or path relative to the project root
    #[arg(short = 'F', long)]
    filter: Option<String>,
    /// Override package-manager detection
    #[arg(long, value_enum)]
    package_manager: Option<project::Manager>,
    /// Emit one JSON result per request, including failures
    #[arg(long)]
    json: bool,
    /// Override the platform cache directory
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct Resolved {
    name: String,
    version: String,
    #[serde(flatten)]
    source: source::Source,
}

fn resolve(
    spec: &str,
    args: &Args,
    root: &std::path::Path,
    manager: project::Manager,
    projects: &mut Option<Vec<project::Project>>,
    cache: &std::path::Path,
) -> Result<Resolved> {
    let (requested_name, requested_version) = project::parse_spec(spec)?;
    let (name, version, manifest) = if let Some(version) = requested_version {
        (requested_name.to_owned(), version.to_owned(), Value::Null)
    } else {
        if projects.is_none() {
            *projects = Some(project::load_projects(root, manager)?);
        }
        let dependency = project::select_dependency(
            projects.as_deref().unwrap_or_default(),
            root,
            args.filter.as_deref(),
            requested_name,
        )?;
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
        (name, version, manifest)
    };
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
        source,
    })
}

fn run(args: &Args) -> Result<bool> {
    let cwd = dunce::canonicalize(std::env::current_dir()?)?;
    let (root, manager) = project::discover(&cwd, args.package_manager)?;
    let cache = args
        .cache_dir
        .clone()
        .or_else(|| {
            ProjectDirs::from("", "", "paths-cli").map(|dirs| dirs.cache_dir().to_path_buf())
        })
        .context("cannot determine cache directory; use --cache-dir")?;
    fs::create_dir_all(&cache)?;
    let cache = dunce::canonicalize(cache)?;
    let mut projects = None;
    let mut failed = false;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for spec in &args.dependencies {
        match resolve(spec, args, &root, manager, &mut projects, &cache) {
            Ok(resolved) => {
                if args.json {
                    writeln!(
                        output,
                        "{}",
                        serde_json::json!({"request": spec, "result": resolved})
                    )?;
                } else {
                    writeln!(output, "{}", resolved.source.path.display())?;
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
    Ok(failed)
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
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
