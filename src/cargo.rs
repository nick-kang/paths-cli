use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use semver::Version;
use serde::Deserialize;

use crate::{Resolved, ResolvedSource, command, project::Manager};

#[derive(Default)]
pub struct Resolver {
    default: Option<std::result::Result<Metadata, String>>,
    all_features: Option<std::result::Result<Metadata, String>>,
}

impl Resolver {
    pub fn resolve(
        &mut self,
        root: &Path,
        filter: Option<&str>,
        name: &str,
        version: Option<&str>,
    ) -> Result<Resolved> {
        let metadata = self.metadata(root, false)?;
        let (primary, fallback) = workspace_roots(metadata, filter)?;
        if let Some(package) = find(metadata, &primary, name, version)? {
            return resolved(package);
        }
        let metadata = self.metadata(root, true)?;
        if let Some(package) = find(metadata, &primary, name, version)? {
            return resolved(package);
        }
        if !fallback.is_empty() {
            eprintln!(
                "warning: {name} is absent from {}; searching other workspaces",
                filter.unwrap_or("the selected Cargo project")
            );
            if let Some(package) = find(metadata, &fallback, name, version)? {
                return resolved(package);
            }
        }
        let request =
            version.map_or_else(|| name.to_owned(), |version| format!("{name}@{version}"));
        bail!("resolved Cargo dependency `{request}` not found")
    }

    fn metadata(&mut self, root: &Path, all_features: bool) -> Result<&Metadata> {
        let slot = if all_features {
            &mut self.all_features
        } else {
            &mut self.default
        };
        slot.get_or_insert_with(|| load(root, all_features).map_err(|error| format!("{error:#}")))
            .as_ref()
            .map_err(|message| anyhow!("{message}"))
    }
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    #[serde(default)]
    workspace_default_members: Vec<String>,
    resolve: Option<Graph>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    manifest_path: PathBuf,
}

#[derive(Deserialize)]
struct Graph {
    root: Option<String>,
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    dependencies: Vec<String>,
    #[serde(default)]
    deps: Vec<Edge>,
}

#[derive(Deserialize)]
struct Edge {
    name: String,
    pkg: String,
}

fn load(root: &Path, all_features: bool) -> Result<Metadata> {
    let mut command = Manager::Cargo.command(root)?;
    command.args(["metadata", "--format-version", "1", "--locked"]);
    if all_features {
        command.arg("--all-features");
    }
    let text = command::checked(&mut command).map_err(|error| {
        let message = format!("{error:#}");
        if message.contains("Cargo.lock") || message.contains("lock file") {
            error.context("Cargo.lock must be current; run `cargo fetch`, then retry")
        } else {
            error
        }
    })?;
    serde_json::from_str(&text).context("invalid Cargo metadata")
}

fn workspace_roots(
    metadata: &Metadata,
    filter: Option<&str>,
) -> Result<(Vec<String>, Vec<String>)> {
    let graph = metadata
        .resolve
        .as_ref()
        .context("missing resolve in Cargo metadata")?;
    let mut members: Vec<_> = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .collect();
    members.sort_by(|a, b| a.manifest_path.parent().cmp(&b.manifest_path.parent()));
    let primary = if let Some(filter) = filter {
        let filter_path = dunce::canonicalize(metadata.workspace_root.join(filter)).ok();
        let selected: Vec<_> = members
            .iter()
            .filter(|package| {
                package.name == filter || filter_path.as_deref() == package.manifest_path.parent()
            })
            .collect();
        ensure!(
            selected.len() == 1,
            "workspace filter `{filter}` matched {} projects; expected exactly one",
            selected.len()
        );
        vec![selected[0].id.clone()]
    } else if let Some(root) = &graph.root {
        vec![root.clone()]
    } else {
        members
            .iter()
            .filter(|package| {
                metadata.workspace_default_members.is_empty()
                    || metadata.workspace_default_members.contains(&package.id)
            })
            .map(|package| package.id.clone())
            .collect()
    };
    ensure!(!primary.is_empty(), "no Cargo workspace members found");
    let fallback = members
        .into_iter()
        .filter(|package| !primary.contains(&package.id))
        .map(|package| package.id.clone())
        .collect();
    Ok((primary, fallback))
}

fn find<'a>(
    metadata: &'a Metadata,
    roots: &[String],
    name: &str,
    version: Option<&str>,
) -> Result<Option<&'a Package>> {
    let graph = metadata
        .resolve
        .as_ref()
        .context("missing resolve in Cargo metadata")?;
    let nodes: HashMap<_, _> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let alias = name.replace('-', "_");
    for root in roots {
        let mut depths = HashMap::from([(root.as_str(), 0_usize)]);
        let mut aliases = HashMap::new();
        let mut queue = VecDeque::from([root.as_str()]);
        while let Some(id) = queue.pop_front() {
            let depth = depths[id];
            if let Some(node) = nodes.get(id) {
                for edge in &node.deps {
                    if edge.name == alias {
                        aliases.entry(edge.pkg.as_str()).or_insert(depth + 1);
                    }
                }
                for dependency in &node.dependencies {
                    if !depths.contains_key(dependency.as_str()) {
                        depths.insert(dependency.as_str(), depth + 1);
                        queue.push_back(dependency.as_str());
                    }
                }
            }
        }
        let mut matches = Vec::new();
        for package in &metadata.packages {
            if version.is_some_and(|version| package.version != version) {
                continue;
            }
            let depth = if package.name == name {
                depths.get(package.id.as_str()).copied()
            } else {
                aliases.get(package.id.as_str()).copied()
            };
            if let Some(depth) = depth {
                let version =
                    Version::parse(&package.version).context("invalid Cargo package version")?;
                matches.push((depth, version, package));
            }
        }
        if let Some((_, _, package)) = matches.into_iter().min_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp_precedence(&b.1))
                .then(a.2.manifest_path.parent().cmp(&b.2.manifest_path.parent()))
        }) {
            return Ok(Some(package));
        }
    }
    Ok(None)
}

fn resolved(package: &Package) -> Result<Resolved> {
    let path = package
        .manifest_path
        .parent()
        .context("Cargo manifest has no parent directory")?;
    ensure!(path.is_absolute(), "Cargo source path is not absolute");
    Ok(Resolved {
        name: package.name.clone(),
        version: package.version.clone(),
        source: ResolvedSource::Cargo {
            path: dunce::canonicalize(path)
                .with_context(|| format!("cannot read Cargo source {}", path.display()))?,
            method: "cargo",
        },
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};

    use super::*;

    fn package(id: &str, name: &str, version: &str, path: &Path) -> Value {
        json!({"id": id, "name": name, "version": version, "manifest_path": path.join("Cargo.toml")})
    }

    fn node(id: &str, dependencies: &[(&str, &str)]) -> Value {
        json!({
            "id": id,
            "dependencies": dependencies.iter().map(|(_, id)| id).collect::<Vec<_>>(),
            "deps": dependencies.iter().map(|(name, id)| json!({"name": name, "pkg": id})).collect::<Vec<_>>()
        })
    }

    fn fixture(packages: &[Value], nodes: &[Value]) -> Result<Metadata> {
        Ok(serde_json::from_value(json!({
            "packages": packages,
            "workspace_root": "/workspace",
            "workspace_members": ["root"],
            "workspace_default_members": ["root"],
            "resolve": {"root": "root", "nodes": nodes}
        }))?)
    }

    #[test]
    fn chooses_nearest_then_lowest_version_and_path_with_cycles() -> Result<()> {
        let metadata = fixture(
            &[
                package("root", "app", "1.0.0", Path::new("/workspace")),
                package("branch", "branch", "1.0.0", Path::new("/branch")),
                package("far", "crate", "1.0.0", Path::new("/far")),
                package("new", "crate", "3.0.0", Path::new("/new")),
                package("z", "crate", "2.0.0", Path::new("/a/0-child")),
                package("a", "crate", "2.0.0", Path::new("/a")),
            ],
            &[
                node(
                    "root",
                    &[
                        ("branch", "branch"),
                        ("crate", "z"),
                        ("crate", "new"),
                        ("crate", "a"),
                    ],
                ),
                node("branch", &[("crate", "far"), ("app", "root")]),
                node("far", &[("branch", "branch")]),
            ],
        )?;
        let roots = vec!["root".to_owned()];
        assert_eq!(
            find(&metadata, &roots, "crate", None)?.map(|package| package.id.as_str()),
            Some("a")
        );
        assert_eq!(
            find(&metadata, &roots, "crate", Some("1.0.0"))?.map(|package| package.id.as_str()),
            Some("far")
        );
        assert!(find(&metadata, &roots, "crate", Some("4.0.0"))?.is_none());
        assert_eq!(
            find(&metadata, &roots, "app", None)?.map(|package| package.id.as_str()),
            Some("root")
        );
        Ok(())
    }

    #[test]
    fn aliases_normalize_hyphens_without_normalizing_package_names() -> Result<()> {
        let metadata = fixture(
            &[
                package("root", "app", "1.0.0", Path::new("/workspace")),
                package("dep", "serde-json", "1.0.0", Path::new("/dep")),
            ],
            &[node("root", &[("json_alias", "dep")])],
        )?;
        let roots = vec!["root".to_owned()];
        for name in ["serde-json", "json_alias", "json-alias"] {
            assert_eq!(
                find(&metadata, &roots, name, None)?.map(|package| package.id.as_str()),
                Some("dep")
            );
        }
        assert!(find(&metadata, &roots, "serde_json", None)?.is_none());
        Ok(())
    }

    #[test]
    fn aliases_are_ranked_by_the_matching_edge_depth() -> Result<()> {
        let metadata = fixture(
            &[
                package("root", "app", "1.0.0", Path::new("/workspace")),
                package("branch", "branch", "1.0.0", Path::new("/branch")),
                package("reused", "crate", "1.0.0", Path::new("/reused")),
                package("direct", "crate", "2.0.0", Path::new("/direct")),
            ],
            &[
                node(
                    "root",
                    &[
                        ("other", "reused"),
                        ("branch", "branch"),
                        ("wanted", "direct"),
                    ],
                ),
                node("branch", &[("wanted", "reused")]),
            ],
        )?;
        assert_eq!(
            find(&metadata, &["root".to_owned()], "wanted", None)?
                .map(|package| package.id.as_str()),
            Some("direct")
        );
        Ok(())
    }

    #[test]
    fn workspace_defaults_and_fallbacks_are_sorted_and_filters_are_exact() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let directory = dunce::canonicalize(directory.path())?;
        fs::create_dir(directory.join("a"))?;
        fs::create_dir_all(directory.join("a/0-child"))?;
        let mut metadata = fixture(
            &[
                package("b", "second", "1.0.0", &directory.join("a/0-child")),
                package("a", "first", "1.0.0", &directory.join("a")),
            ],
            &[node("a", &[]), node("b", &[])],
        )?;
        metadata.workspace_root = directory;
        metadata.workspace_members = vec!["b".to_owned(), "a".to_owned()];
        metadata.workspace_default_members = vec!["b".to_owned(), "a".to_owned()];
        metadata
            .resolve
            .as_mut()
            .context("missing test graph")?
            .root = None;
        assert_eq!(
            workspace_roots(&metadata, None)?,
            (vec!["a".to_owned(), "b".to_owned()], vec![])
        );
        for filter in ["first", "./a"] {
            assert_eq!(
                workspace_roots(&metadata, Some(filter))?,
                (vec!["a".to_owned()], vec!["b".to_owned()])
            );
        }
        assert!(workspace_roots(&metadata, Some("fir*")).is_err());
        Ok(())
    }

    #[test]
    fn all_features_primary_precedes_other_workspaces() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let directory = dunce::canonicalize(directory.path())?;
        for name in ["a", "b", "optional", "other"] {
            fs::create_dir(directory.join(name))?;
        }
        let packages = vec![
            package("a", "first", "1.0.0", &directory.join("a")),
            package("b", "second", "1.0.0", &directory.join("b")),
            package("optional", "dep", "1.0.0", &directory.join("optional")),
            package("other", "dep", "2.0.0", &directory.join("other")),
        ];
        let mut default = fixture(&packages, &[node("a", &[]), node("b", &[("dep", "other")])])?;
        let mut all_features = fixture(
            &packages,
            &[
                node("a", &[("dep", "optional")]),
                node("b", &[("dep", "other")]),
            ],
        )?;
        for metadata in [&mut default, &mut all_features] {
            metadata.workspace_root.clone_from(&directory);
            metadata.workspace_members = vec!["a".to_owned(), "b".to_owned()];
            metadata
                .resolve
                .as_mut()
                .context("missing test graph")?
                .root = Some("a".to_owned());
        }
        let mut resolver = Resolver {
            default: Some(Ok(default)),
            all_features: Some(Ok(all_features)),
        };
        let result = resolver.resolve(&directory, None, "dep", None)?;
        let json = serde_json::to_value(result)?;
        assert_eq!(
            json,
            json!({"name": "dep", "version": "1.0.0", "path": directory.join("optional"), "method": "cargo"})
        );
        let result = resolver.resolve(&directory, None, "dep", Some("2.0.0"))?;
        assert_eq!(result.version, "2.0.0");
        assert!(
            resolver
                .resolve(&directory, Some("missing"), "dep", None)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn cached_feature_failure_preserves_default_graph_success() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let default = fixture(
            &[package("root", "app", "1.0.0", directory.path())],
            &[node("root", &[])],
        )?;
        let mut resolver = Resolver {
            default: Some(Ok(default)),
            all_features: Some(Err("optional feature graph unavailable".to_owned())),
        };
        for _ in 0..2 {
            let error = resolver
                .resolve(directory.path(), None, "missing", None)
                .err()
                .context("expected cached failure")?;
            assert_eq!(error.to_string(), "optional feature graph unavailable");
            let result = resolver.resolve(directory.path(), None, "app", None)?;
            assert_eq!(result.name, "app");
        }
        Ok(())
    }
}
