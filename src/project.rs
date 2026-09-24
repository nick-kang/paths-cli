use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use semver::Version;
use serde_json::Value;

use crate::command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Manager {
    Npm,
    Pnpm,
}

impl Manager {
    pub fn executable(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
        }
    }

    fn command(self, root: &Path) -> Result<Command> {
        let executable = which::which(self.executable())
            .with_context(|| format!("{} must be installed and on PATH", self.executable()))?;
        let mut command = Command::new(executable);
        command.current_dir(root).env("NO_COLOR", "1");
        Ok(command)
    }
}

pub fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(
        &fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
    )
    .with_context(|| format!("invalid JSON in {}", path.display()))
}

pub fn discover(cwd: &Path, override_manager: Option<Manager>) -> Result<(PathBuf, Manager)> {
    let mut nearest = None;
    let mut root = None;
    for directory in cwd.ancestors() {
        let manifest = directory.join("package.json");
        if manifest.is_file() {
            nearest.get_or_insert_with(|| directory.to_path_buf());
            if read_json(&manifest)?.get("workspaces").is_some() {
                root = Some(directory.to_path_buf());
                break;
            }
        }
        if directory.join("pnpm-workspace.yaml").is_file() {
            root = Some(directory.to_path_buf());
            break;
        }
        if directory.join(".git").exists() {
            break;
        }
    }
    let root = root
        .or(nearest)
        .context("no package.json found in this directory or its parents")?;
    if let Some(manager) = override_manager {
        return Ok((root, manager));
    }
    let manifest_path = root.join("package.json");
    let manifest = if manifest_path.is_file() {
        read_json(&manifest_path)?
    } else {
        Value::Null
    };
    if let Some(declaration) = manifest["packageManager"].as_str() {
        let manager = match declaration.split('@').next() {
            Some("npm") => Manager::Npm,
            Some("pnpm") => Manager::Pnpm,
            _ => {
                bail!("unsupported package manager {declaration}; only npm and pnpm are supported")
            }
        };
        return Ok((root, manager));
    }
    let pnpm = root.join("pnpm-lock.yaml").exists() || root.join("pnpm-workspace.yaml").exists();
    let npm = root.join("package-lock.json").exists() || root.join("npm-shrinkwrap.json").exists();
    ensure!(
        !(root.join("yarn.lock").exists() || pnpm && npm),
        "ambiguous or unsupported package manager; use --package-manager npm|pnpm"
    );
    let manager = match (pnpm, npm) {
        (true, false) => Manager::Pnpm,
        (false, true) => Manager::Npm,
        _ => bail!("cannot detect package manager; use --package-manager npm|pnpm"),
    };
    Ok((root, manager))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub depth: usize,
    pub path: PathBuf,
    pub version: String,
}

pub fn find_dependency(root: &Value, name: &str) -> Option<Dependency> {
    let mut queue = VecDeque::from([(root, 0)]);
    let mut matches = Vec::new();
    while let Some((node, depth)) = queue.pop_front() {
        for group in ["dependencies", "devDependencies", "optionalDependencies"] {
            if let Some(children) = node[group].as_object() {
                for (alias, child) in children {
                    if !child.is_object() {
                        continue;
                    }
                    if (alias == name
                        || child["name"].as_str() == Some(name)
                        || child["from"].as_str() == Some(name))
                        && child["missing"] != true
                        && child["extraneous"] != true
                        && let (Some(path), Some(version)) =
                            (child["path"].as_str(), child["version"].as_str())
                    {
                        matches.push(Dependency {
                            depth: depth + 1,
                            path: PathBuf::from(path),
                            version: version.to_owned(),
                        });
                    }
                    queue.push_back((child, depth + 1));
                }
            }
        }
    }
    matches.into_iter().min_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(
                || match (Version::parse(&a.version), Version::parse(&b.version)) {
                    (Ok(a), Ok(b)) => a.cmp_precedence(&b),
                    _ => a.version.cmp(&b.version),
                },
            )
            .then(a.path.cmp(&b.path))
    })
}

pub struct Project {
    pub path: PathBuf,
    pub name: String,
    pub graph: Value,
}

pub fn load_projects(root: &Path, manager: Manager) -> Result<Vec<Project>> {
    let mut cmd = manager.command(root)?;
    let graphs = match manager {
        Manager::Pnpm => {
            cmd.args(["--recursive", "list", "--depth", "Infinity", "--json"]);
            let text = command::checked(&mut cmd)?;
            serde_json::from_str::<Vec<Value>>(&text).context("invalid pnpm dependency graph")?
        }
        Manager::Npm => {
            let manifest = read_json(&root.join("package.json"))?;
            cmd.args([
                "ls",
                "--all",
                "--long",
                "--json",
                "--include=dev",
                "--include=optional",
            ]);
            let workspaces = npm_workspace_paths(root, &manifest)?;
            if !workspaces.is_empty() {
                cmd.args(["--workspaces", "--include-workspace-root"]);
            }
            let output = command::run(&mut cmd)?;
            let mut graph: Value =
                serde_json::from_slice(&output.stdout).context("invalid npm dependency graph")?;
            if !output.status.success() {
                // npm can return a usable graph with ELSPROBLEMS for an unrelated missing dependency.
                ensure!(
                    graph["error"]["code"] == "ELSPROBLEMS",
                    "npm ls failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                eprintln!(
                    "warning: npm reports an incomplete installation; using available dependencies"
                );
            }
            let mut graphs = Vec::new();
            if let Some(dependencies) = graph["dependencies"].as_object_mut() {
                let names: Vec<_> = dependencies
                    .iter()
                    .filter_map(|(name, node)| {
                        node["path"]
                            .as_str()
                            .and_then(|path| dunce::canonicalize(path).ok())
                            .filter(|path| workspaces.contains(path))
                            .map(|_| name.clone())
                    })
                    .collect();
                for name in names {
                    if let Some(node) = dependencies.remove(&name) {
                        graphs.push(node);
                    }
                }
            }
            graphs.insert(0, graph);
            graphs
        }
    };
    let mut projects: Vec<_> = graphs
        .into_iter()
        .filter_map(|graph| {
            let path = dunce::canonicalize(graph["path"].as_str()?).ok()?;
            let name = graph["name"].as_str().unwrap_or_default().to_owned();
            Some(Project { path, name, graph })
        })
        .collect();
    projects.sort_by(|a, b| a.path.cmp(&b.path));
    ensure!(
        !projects.is_empty(),
        "no installed projects found; install dependencies with {} first",
        manager.executable()
    );
    Ok(projects)
}

fn npm_workspace_paths(root: &Path, manifest: &Value) -> Result<Vec<PathBuf>> {
    let workspaces = manifest["workspaces"]
        .as_array()
        .or_else(|| manifest["workspaces"]["packages"].as_array());
    let mut paths = Vec::new();
    for pattern in workspaces.into_iter().flatten().filter_map(Value::as_str) {
        let pattern = format!(
            "{}/{pattern}/package.json",
            glob::Pattern::escape(&root.to_string_lossy())
        );
        for entry in glob::glob(&pattern)? {
            let entry = entry?;
            if let Some(parent) = entry.parent() {
                paths.push(dunce::canonicalize(parent)?);
            }
        }
    }
    Ok(paths)
}

pub fn select_dependency(
    projects: &[Project],
    root: &Path,
    filter: Option<&str>,
    name: &str,
) -> Result<Dependency> {
    if let Some(filter) = filter {
        let filter_path = dunce::canonicalize(root.join(filter)).ok();
        let selected: Vec<_> = projects
            .iter()
            .filter(|project| project.name == filter || filter_path.as_ref() == Some(&project.path))
            .collect();
        ensure!(
            selected.len() == 1,
            "workspace filter `{filter}` matched {} projects; expected exactly one",
            selected.len()
        );
        if let Some(dependency) = find_dependency(&selected[0].graph, name) {
            return validate_dependency(dependency);
        }
        eprintln!("warning: {name} is absent from {filter}; searching other workspaces");
    }
    for project in projects {
        if let Some(dependency) = find_dependency(&project.graph, name) {
            return validate_dependency(dependency);
        }
    }
    bail!("installed dependency `{name}` not found")
}

fn validate_dependency(dependency: Dependency) -> Result<Dependency> {
    ensure!(
        !["file:", "link:", "workspace:"]
            .iter()
            .any(|prefix| dependency.version.starts_with(prefix)),
        "local workspace dependencies do not have published source revisions"
    );
    Ok(dependency)
}

pub fn metadata(root: &Path, manager: Manager, name: &str, version: &str) -> Result<Value> {
    let text = command::checked(manager.command(root)?.args([
        "view",
        &format!("{name}@{version}"),
        "--json",
    ]))?;
    let value: Value = serde_json::from_str(&text).context("invalid registry metadata")?;
    ensure!(
        value["version"].as_str() == Some(version),
        "registry returned a different version for {name}@{version}"
    );
    Ok(value)
}

pub fn parse_spec(spec: &str) -> Result<(&str, Option<&str>)> {
    let split = spec.rfind('@').filter(|index| *index > 0);
    let (name, version) = split.map_or((spec, None), |index| {
        (&spec[..index], Some(&spec[index + 1..]))
    });
    let parts: Vec<_> = name.strip_prefix('@').unwrap_or(name).split('/').collect();
    ensure!(
        parts.len() == if name.starts_with('@') { 2 } else { 1 },
        "invalid package name `{name}`"
    );
    ensure!(
        parts.iter().all(|part| !part.is_empty()
            && !part.starts_with(['.', '-'])
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))),
        "invalid package name `{name}`"
    );
    if let Some(version) = version {
        Version::parse(version).context("use an exact version, for example react@19.0.0")?;
    }
    Ok((name, version))
}
