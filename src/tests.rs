use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, anyhow};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde_json::{Value, json};

use crate::{command, project, source};

const REPOSITORY: &str = "https://github.com/example/source";
const COMMIT: &str = "748058e837d9c4247330e3d45580cbdae52bffda";

#[test]
fn dependency_selection_and_workspace_fallback() -> Result<()> {
    let graph = json!({"dependencies": {
        "parent": {"dependencies": {"target": {"path": "/nested", "version": "0.5.0"}}},
        "alias": {"from": "target", "path": "/alias", "version": "2.0.0"}
    }, "devDependencies": {"target": {"path": "/direct", "version": "1.0.0"}}});
    assert_eq!(
        project::find_dependency(&graph, "target")
            .context("missing dependency")?
            .path,
        Path::new("/direct")
    );
    let alias = json!({"dependencies": {"alias": {"name": "target", "path": "/alias", "version": "1.0.0"}}});
    assert!(project::find_dependency(&alias, "target").is_some());
    assert!(project::find_dependency(&alias, "alias").is_some());
    let first = json!({"optionalDependencies": {"parent": {"dependencies": {
        "@scope/target": {"path": "/transitive", "version": "1.0.0"}
    }}}});
    assert_eq!(
        project::find_dependency(&first, "@scope/target")
            .context("missing scoped dependency")?
            .depth,
        2
    );
    let projects = vec![
        project::Project {
            name: "first".into(),
            path: "/first".into(),
            graph: first,
        },
        project::Project {
            name: "second".into(),
            path: "/second".into(),
            graph: json!({"dependencies": {"@scope/target": {"path": "/direct", "version": "2.0.0"}}}),
        },
        project::Project {
            name: "empty".into(),
            path: "/empty".into(),
            graph: json!({}),
        },
    ];
    for filter in [None, Some("empty")] {
        let selected =
            project::select_dependency(&projects, Path::new("/"), filter, "@scope/target")?;
        assert_eq!(selected.path, Path::new("/transitive"));
    }
    assert!(
        project::select_dependency(&projects, Path::new("/"), Some("unknown"), "@scope/target")
            .is_err()
    );
    assert!(project::find_dependency(&Value::Null, "missing").is_none());
    Ok(())
}

#[test]
fn repeated_graph_nodes_keep_the_shallowest_match() -> Result<()> {
    let target = json!({"path": "/same", "version": "1.0.0"});
    let graph =
        json!({"dependencies": {"target": target, "parent": {"dependencies": {"target": target}}}});
    assert_eq!(
        project::find_dependency(&graph, "target")
            .context("missing")?
            .depth,
        1
    );
    let graph = json!({"dependencies": {"a": {"name": "target", "path": "/release", "version": "1.0.0"}, "b": {"name": "target", "path": "/pre", "version": "1.0.0-beta.1"}}});
    assert_eq!(
        project::find_dependency(&graph, "target")
            .context("missing prerelease")?
            .path,
        Path::new("/pre")
    );
    Ok(())
}

#[test]
fn manager_detection_and_specs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    fs::write(
        root.join("package.json"),
        r#"{"workspaces":["packages/*"]}"#,
    )?;
    fs::create_dir_all(root.join("packages/web/src"))?;
    fs::write(root.join("packages/web/package.json"), "{}")?;
    fs::write(root.join("package-lock.json"), "{}")?;
    assert_eq!(
        project::discover(&root.join("packages/web/src"), None)?.1,
        project::Manager::Npm
    );
    fs::write(root.join("pnpm-lock.yaml"), "")?;
    assert!(project::discover(root, None).is_err());
    assert_eq!(
        project::discover(root, Some(project::Manager::Pnpm))?.1,
        project::Manager::Pnpm
    );
    fs::write(
        root.join("package.json"),
        r#"{"packageManager":"pnpm@10.0.0"}"#,
    )?;
    assert_eq!(project::discover(root, None)?.1, project::Manager::Pnpm);
    fs::write(
        root.join("package.json"),
        r#"{"packageManager":"yarn@4.0.0"}"#,
    )?;
    assert!(project::discover(root, None).is_err());
    assert_eq!(
        project::parse_spec("@scope/name@1.2.3")?,
        ("@scope/name", Some("1.2.3"))
    );
    for invalid in [
        "--help",
        "../name",
        "@scope",
        "name@latest",
        "name;echo",
        "name@",
        "a/b",
    ] {
        assert!(project::parse_spec(invalid).is_err(), "{invalid}");
    }
    Ok(())
}

#[test]
fn repository_urls_and_commits() {
    for input in [
        "example/source",
        "github:example/source",
        "git@github.com:example/source.git",
        "git+https://github.com/example/source.git",
        "ssh://git@github.com/example/source.git",
    ] {
        assert_eq!(
            source::normalize_repository(&json!(input)).as_deref(),
            Some(REPOSITORY),
            "{input}"
        );
    }
    assert_eq!(
        source::normalize_repository(&json!({"url": REPOSITORY})).as_deref(),
        Some(REPOSITORY)
    );
    for input in [
        "not a URL",
        "https://github.com/source",
        "https://github.com/owner/%2fescape",
        "file:///tmp/a/b",
        "ext::command",
        "https://user:secret@host/owner/repo",
    ] {
        assert!(
            source::normalize_repository(&json!(input)).is_none(),
            "{input}"
        );
    }
    assert_eq!(
        source::normalize_commit(&COMMIT.to_uppercase()).as_deref(),
        Some(COMMIT)
    );
    for invalid in ["a".repeat(41), "g".repeat(40), "a".repeat(39)] {
        assert!(source::normalize_commit(&invalid).is_none());
    }
    assert!(source::normalize_commit(&"a".repeat(64)).is_some());
}

fn attestations(workflow: &str, dependency: &str, commit: &str) -> Value {
    let statement = json!({"predicateType": "https://slsa.dev/provenance/v1", "predicate": {"buildDefinition": {
        "externalParameters": {"workflow": {"repository": workflow}},
        "resolvedDependencies": [{"uri": format!("git+{dependency}@refs/heads/beta"), "digest": {"gitCommit": commit}}]
    }}});
    json!({"attestations": [{"predicateType": "https://slsa.dev/provenance/v1", "bundle": {"dsseEnvelope": {"payload": BASE64_STANDARD.encode(statement.to_string())}}}]})
}

#[test]
fn provenance_and_invalid_metadata() {
    assert_eq!(
        source::provenance_commit(&attestations(REPOSITORY, REPOSITORY, COMMIT), REPOSITORY)
            .as_deref(),
        Some(COMMIT)
    );
    for response in [
        attestations("https://github.com/other/repo", REPOSITORY, COMMIT),
        attestations(REPOSITORY, "https://github.com/other/repo", COMMIT),
        attestations(REPOSITORY, REPOSITORY, "not-a-commit"),
        json!({"attestations": [{"predicateType": "https://slsa.dev/provenance/v1", "bundle": {"dsseEnvelope": {"payload": "not-json"}}}]}),
        Value::Null,
    ] {
        assert!(source::provenance_commit(&response, REPOSITORY).is_none());
    }
    assert!(source::published_at(&json!({"time": {"1.0.0": "invalid"}}), "1.0.0").is_none());
    assert!(
        source::published_at(&json!({"time": {"2.0.0": "2025-01-01T00:00:00Z"}}), "1.0.0")
            .is_none()
    );
    assert_eq!(
        source::published_at(
            &json!({"time": {"1.0.0": "2025-01-01T01:00:00+01:00"}}),
            "1.0.0"
        )
        .as_deref(),
        Some("2025-01-01T00:00:00.000Z")
    );
}

#[test]
fn lightweight_annotated_and_conflicting_tags() {
    let other = "b".repeat(40);
    assert_eq!(
        source::tag_commit(&format!("{COMMIT}\trefs/tags/v1.0.0\n"), "1.0.0").as_deref(),
        Some(COMMIT)
    );
    assert_eq!(
        source::tag_commit(
            &format!("{other}\trefs/tags/v1.0.0\n{COMMIT}\trefs/tags/v1.0.0^{{}}\n"),
            "1.0.0"
        )
        .as_deref(),
        Some(COMMIT)
    );
    assert!(source::tag_commit(&format!("{COMMIT}\trefs/tags/v1.0.1"), "1.0.0").is_none());
    assert!(
        source::tag_commit(
            &format!("{COMMIT}\trefs/tags/v1.0.0\n{other}\trefs/tags/v1.0.0"),
            "1.0.0"
        )
        .is_none()
    );
}

#[test]
fn only_missing_revisions_allow_fallback() {
    for text in [
        "upload-pack: not our ref abc",
        "couldn't find remote ref abc",
        "Server does not allow request for unadvertised object abc",
        "remote ref abc not found",
        "no such remote ref abc",
    ] {
        assert!(source::revision_unavailable(
            &anyhow!(text.to_owned()).context("fetch failed")
        ));
    }
    for text in [
        "Could not resolve host",
        "Permission denied",
        "No space left on device",
    ] {
        assert!(!source::revision_unavailable(&anyhow!(text.to_owned())));
    }
}

fn commit(path: &Path, filename: &str, date: &str) -> Result<String> {
    fs::write(path.join(filename), date)?;
    source::git(path, &["add", filename])?;
    command::checked(
        Command::new("git")
            .current_dir(path)
            .args([
                "-c",
                "core.hooksPath=",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                filename,
            ])
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date),
    )?;
    source::git(path, &["rev-parse", "HEAD"])
}

fn repository() -> Result<tempfile::TempDir> {
    let temp = tempfile::tempdir()?;
    source::git(temp.path(), &["init", "--quiet", "--initial-branch=main"])?;
    source::git(temp.path(), &["config", "user.email", "paths@example.test"])?;
    source::git(temp.path(), &["config", "user.name", "Paths Test"])?;
    Ok(temp)
}

#[test]
fn preferred_commit_is_cloned_even_when_tag_is_cached() -> Result<()> {
    let repo = repository()?;
    let cache = tempfile::tempdir()?;
    let repo_url = repo.path().to_str().context("non-UTF8 fixture path")?;
    let tag = commit(repo.path(), "tag.rs", "2025-01-01T00:00:00Z")?;
    source::git(repo.path(), &["tag", "v1.0.0"])?;
    let tagged_path = source::checkout(cache.path(), repo_url, &tag)?;
    let published = commit(repo.path(), "published.rs", "2025-01-02T00:00:00Z")?;
    let result = source::materialize(
        cache.path(),
        repo_url,
        "1.0.0",
        &json!({"gitHead": published.to_uppercase(), "dist": {"attestations": {"url": "https://invalid.invalid/should-not-be-fetched"}}}),
    )?;
    assert_eq!(result.method, source::Method::Published);
    assert_eq!(result.commit, published);
    assert_ne!(result.path, tagged_path);
    assert_eq!(source::git(&tagged_path, &["rev-parse", "HEAD"])?, tag);
    let fallback =
        source::materialize(cache.path(), repo_url, "1.0.0", &json!({"gitHead": COMMIT}))?;
    assert_eq!(fallback.method, source::Method::Tag);
    assert_eq!(fallback.path, tagged_path);
    fs::write(result.path.join("published.rs"), "modified")?;
    assert!(source::checkout(cache.path(), repo_url, &published).is_err());
    assert_eq!(
        fs::read_to_string(result.path.join("published.rs"))?,
        "modified"
    );
    Ok(())
}

#[test]
fn historical_and_default_branch_fallbacks() -> Result<()> {
    let repo = repository()?;
    let cache = tempfile::tempdir()?;
    let repo_url = repo.path().to_str().context("non-UTF8 fixture path")?;
    commit(repo.path(), "base.rs", "2025-01-01T00:00:00Z")?;
    source::git(repo.path(), &["checkout", "--quiet", "-b", "feature"])?;
    commit(repo.path(), "feature.rs", "2025-01-03T12:00:00Z")?;
    source::git(repo.path(), &["checkout", "--quiet", "main"])?;
    let historical = commit(repo.path(), "main.rs", "2025-01-03T00:00:00Z")?;
    commit(repo.path(), "later.rs", "2025-01-05T00:00:00Z")?;
    command::checked(
        Command::new("git")
            .current_dir(repo.path())
            .args([
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-ff",
                "feature",
                "-m",
                "merge feature",
            ])
            .env("GIT_AUTHOR_DATE", "2025-01-06T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2025-01-06T00:00:00Z"),
    )?;
    assert_eq!(
        source::commit_before_publication(repo_url, "2025-01-04T00:00:00Z")?,
        Some(historical.clone())
    );
    let result = source::materialize(
        cache.path(),
        repo_url,
        "2.0.0",
        &json!({"gitHead": COMMIT, "time": {"2.0.0": "2025-01-04T00:00:00Z"}}),
    )?;
    assert_eq!(result.commit, historical);
    assert_eq!(result.method, source::Method::PublicationTime);
    for metadata in [
        json!({}),
        json!({"time": {"2.0.0": "2020-01-01T00:00:00Z"}}),
    ] {
        let result = source::materialize(cache.path(), repo_url, "2.0.0", &metadata)?;
        assert_eq!(result.method, source::Method::DefaultBranch);
        assert_eq!(
            result.commit,
            source::git(repo.path(), &["rev-parse", "HEAD"])?
        );
    }
    assert!(
        source::materialize(
            cache.path(),
            "/nonexistent/paths-test-repository",
            "1.0.0",
            &json!({"gitHead": COMMIT})
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn source_checkout_excludes_binaries_and_is_concurrency_safe() -> Result<()> {
    let repo = repository()?;
    let cache = tempfile::tempdir()?;
    // Git object paths exceed 260 characters while the checkout root stays launchable.
    let cache_root = cache.path().join("nested-cache-".repeat(2));
    commit(repo.path(), "image.PNG", "2025-01-01T00:00:00Z")?;
    let head = commit(repo.path(), "main.rs", "2025-01-02T00:00:00Z")?;
    let repo_url = repo.path().to_str().context("non-UTF8 fixture path")?;
    let (first, second) = std::thread::scope(|scope| {
        let a = scope.spawn(|| source::checkout(&cache_root, repo_url, &head));
        let b = scope.spawn(|| source::checkout(&cache_root, repo_url, &head));
        (a.join(), b.join())
    });
    let first = first.map_err(|_| anyhow!("checkout thread panicked"))??;
    let second = second.map_err(|_| anyhow!("checkout thread panicked"))??;
    assert_eq!(first, second);
    assert!(!first.join("image.PNG").exists());
    assert!(first.join("main.rs").is_file());
    Ok(())
}
