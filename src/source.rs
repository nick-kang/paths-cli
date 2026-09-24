use std::{
    collections::HashSet,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use chrono::{DateTime, SecondsFormat};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::command;

const PROVENANCE: &str = "https://slsa.dev/provenance/v1";
const BINARY_EXTENSIONS: &str = "png jpg jpeg gif webp avif ico bmp tif tiff psd mp3 mp4 m4a wav ogg oga ogv webm mov avi mkv flac aac woff woff2 ttf otf eot zip tar gz tgz bz2 xz 7z rar exe dll so dylib a lib class jar war wasm pdf doc docx xls xlsx ppt pptx sqlite db bin map";

pub fn normalize_repository(value: &Value) -> Option<String> {
    let raw = value.as_str().or_else(|| value["url"].as_str())?.trim();
    let raw = raw.strip_prefix("git+").unwrap_or(raw);
    let raw = raw.split("@refs/").next()?;
    let expanded = if let Some(path) = raw
        .strip_prefix("github:")
        .or_else(|| raw.strip_prefix("git@github.com:"))
    {
        format!("https://github.com/{path}")
    } else if !raw.contains(':') && raw.split('/').count() == 2 {
        format!("https://github.com/{raw}")
    } else {
        raw.to_owned()
    };
    let mut url = Url::parse(&expanded.replacen("git://", "https://", 1)).ok()?;
    if !matches!(url.scheme(), "https" | "http" | "ssh") || url.password().is_some() {
        return None;
    }
    if url.host_str()? == "github.com" {
        url = Url::parse(&format!("https://github.com{}", url.path())).ok()?;
    }
    if !matches!(url.scheme(), "https" | "http" | "ssh")
        || url.password().is_some()
        || (url.scheme() != "ssh" && !url.username().is_empty())
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).to_owned();
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    if segments.len() < 2
        || segments.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || part.contains('%')
                || part.contains('\\')
                || part.chars().any(char::is_control)
        })
    {
        return None;
    }
    url.set_path(&path);
    url.set_fragment(None);
    url.set_query(None);
    Some(url.to_string().trim_end_matches('/').to_owned())
}

pub fn normalize_commit(value: &str) -> Option<String> {
    (matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

pub fn provenance_commit(response: &Value, repository: &str) -> Option<String> {
    for attestation in response["attestations"].as_array()? {
        if attestation["predicateType"] != PROVENANCE {
            continue;
        }
        let statement = attestation["bundle"]["dsseEnvelope"]["payload"]
            .as_str()
            .and_then(|payload| BASE64_STANDARD.decode(payload).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let Some(statement) = statement else {
            continue;
        };
        let definition = &statement["predicate"]["buildDefinition"];
        if statement["predicateType"] != PROVENANCE
            || normalize_repository(&definition["externalParameters"]["workflow"]["repository"])
                .as_deref()
                != Some(repository)
        {
            continue;
        }
        let commits: HashSet<_> = definition["resolvedDependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|dependency| {
                normalize_repository(&dependency["uri"]).as_deref() == Some(repository)
            })
            .filter_map(|dependency| {
                dependency["digest"]["gitCommit"]
                    .as_str()
                    .and_then(normalize_commit)
            })
            .collect();
        if commits.len() == 1 {
            return commits.into_iter().next();
        }
    }
    None
}

fn published_commit(metadata: &Value, repository: &str) -> Option<String> {
    if let Some(commit) = metadata["gitHead"].as_str().and_then(normalize_commit) {
        return Some(commit);
    }
    let url = metadata["dist"]["attestations"]["url"]
        .as_str()
        .or_else(|| metadata["dist.attestations"]["url"].as_str())?;
    let parsed = Url::parse(url).ok()?;
    if parsed.scheme() != "https" || parsed.password().is_some() || !parsed.username().is_empty() {
        return None;
    }
    let result = (|| -> Result<Value> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(command::TIMEOUT))
            .max_redirects(0)
            .build()
            .into();
        Ok(agent
            .get(url)
            .call()?
            .body_mut()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_json()?)
    })();
    match result {
        Ok(response) => provenance_commit(&response, repository),
        Err(error) => {
            eprintln!("warning: provenance unavailable: {error:#}");
            None
        }
    }
}

pub fn published_at(metadata: &Value, version: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(metadata["time"][version].as_str()?)
        .ok()
        .map(|date| date.to_utc().to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub fn tag_commit(references: &str, version: &str) -> Option<String> {
    let tag = format!("refs/tags/v{version}");
    let peeled = format!("{tag}^{{}}");
    for wanted in [&peeled, &tag] {
        let commits: HashSet<_> = references
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let commit = normalize_commit(parts.next()?)?;
                (parts.next()? == wanted && parts.next().is_none()).then_some(commit)
            })
            .collect();
        if !commits.is_empty() {
            return (commits.len() == 1)
                .then(|| commits.into_iter().next())
                .flatten();
        }
    }
    None
}

pub fn revision_unavailable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string().to_ascii_lowercase();
        [
            "upload-pack: not our ref",
            "couldn't find remote ref",
            "server does not allow request for unadvertised object",
            "no such remote ref",
        ]
        .iter()
        .any(|pattern| text.contains(pattern))
            || (text.contains("remote ref ") && text.contains(" not found"))
    })
}

pub fn git(directory: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    let hooks = if cfg!(windows) {
        "core.hooksPath=NUL"
    } else {
        "core.hooksPath=/dev/null"
    };
    command
        .current_dir(directory)
        .args([
            "-c",
            hooks,
            "-c",
            "core.longpaths=true",
            "-c",
            "filter.lfs.smudge=",
            "-c",
            "filter.lfs.process=",
            "-c",
            "filter.lfs.required=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("LC_ALL", "C");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    command::checked(&mut command)
}

pub fn commit_before_publication(repository: &str, date: &str) -> Result<Option<String>> {
    let temporary = tempfile::tempdir()?;
    git(temporary.path(), &["init", "--bare", "--quiet"])?;
    git(
        temporary.path(),
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--filter=tree:0",
            repository,
            "HEAD",
        ],
    )?;
    // ponytail: publication time is a heuristic; release-branch metadata would improve accuracy.
    let commit = git(
        temporary.path(),
        &[
            "rev-list",
            "--first-parent",
            "--max-count=1",
            &format!("--before={date}"),
            "FETCH_HEAD",
        ],
    )?;
    Ok(normalize_commit(&commit))
}

fn sparse_patterns() -> Vec<String> {
    std::iter::once("/*".to_owned())
        .chain(BINARY_EXTENSIONS.split_whitespace().map(|extension| {
            let pattern: String = extension
                .chars()
                .flat_map(|c| ['[', c, c.to_ascii_uppercase(), ']'])
                .collect();
            format!("!**/*.{pattern}")
        }))
        .collect()
}

fn cache_repository(cache: &Path, repository: &str) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(repository.as_bytes()));
    let label = repository
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo");
    let label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    cache.join("v1").join(format!("{label}-{hash}"))
}

fn current_checkout(path: &Path, repository: &str, commit: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    ensure!(
        path.join(".git").is_dir(),
        "incomplete cache at {}; move it aside and retry",
        path.display()
    );
    ensure!(
        git(path, &["config", "--local", "--get", "remote.origin.url"])? == repository
            && git(path, &["rev-parse", "HEAD"])? == commit,
        "cache identity mismatch at {}; move it aside and retry",
        path.display()
    );
    ensure!(
        git(path, &["status", "--porcelain", "--untracked-files=all"])?.is_empty(),
        "cached source was modified at {}; move it aside and retry",
        path.display()
    );
    Ok(true)
}

pub fn checkout(cache: &Path, repository: &str, commit: &str) -> Result<PathBuf> {
    ensure!(normalize_commit(commit).is_some(), "invalid Git commit");
    let parent = cache_repository(cache, repository);
    fs::create_dir_all(&parent)?;
    let lock = File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(parent.join(".lock"))?;
    lock.lock()?;
    let destination = parent.join(commit);
    if current_checkout(&destination, repository, commit)? {
        return Ok(destination);
    }
    let temporary = tempfile::tempdir_in(&parent)?;
    git(temporary.path(), &["init", "--quiet"])?;
    git(temporary.path(), &["remote", "add", "origin", repository])?;
    git(
        temporary.path(),
        &["config", "remote.origin.promisor", "true"],
    )?;
    git(
        temporary.path(),
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    )?;
    git(
        temporary.path(),
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--depth=1",
            "--filter=blob:none",
            "origin",
            commit,
        ],
    )?;
    let patterns = sparse_patterns();
    let mut sparse = vec!["sparse-checkout", "set", "--no-cone"];
    sparse.extend(patterns.iter().map(String::as_str));
    git(temporary.path(), &sparse)?;
    git(
        temporary.path(),
        &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
    )?;
    ensure!(
        git(temporary.path(), &["rev-parse", "HEAD"])? == commit,
        "fetched revision did not match requested commit"
    );
    fs::rename(temporary.path(), &destination)
        .context("unable to publish source checkout to cache")?;
    Ok(destination)
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    Published,
    Tag,
    PublicationTime,
    DefaultBranch,
}

#[derive(Serialize)]
pub struct Source {
    pub path: PathBuf,
    pub repository: String,
    pub commit: String,
    pub method: Method,
}

fn try_revision(
    cache: &Path,
    repository: &str,
    commit: &str,
    method: Method,
    tried: &mut HashSet<String>,
) -> Result<Option<Source>> {
    if !tried.insert(commit.to_owned()) {
        return Ok(None);
    }
    match checkout(cache, repository, commit) {
        Ok(path) => Ok(Some(Source {
            path,
            repository: repository.to_owned(),
            commit: commit.to_owned(),
            method,
        })),
        Err(error) if revision_unavailable(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn materialize(
    cache: &Path,
    repository: &str,
    version: &str,
    metadata: &Value,
) -> Result<Source> {
    fs::create_dir_all(cache)?;
    let mut tried = HashSet::new();
    if let Some(commit) = published_commit(metadata, repository)
        && let Some(source) =
            try_revision(cache, repository, &commit, Method::Published, &mut tried)?
    {
        return Ok(source);
    }
    let reference = format!("refs/tags/v{version}");
    let references = git(
        cache,
        &[
            "ls-remote",
            "--tags",
            repository,
            &reference,
            &format!("{reference}^{{}}"),
        ],
    )?;
    if let Some(commit) = tag_commit(&references, version)
        && let Some(source) = try_revision(cache, repository, &commit, Method::Tag, &mut tried)?
    {
        return Ok(source);
    }
    if let Some(date) = published_at(metadata, version)
        && let Some(commit) = commit_before_publication(repository, &date)?
        && let Some(source) = try_revision(
            cache,
            repository,
            &commit,
            Method::PublicationTime,
            &mut tried,
        )?
    {
        return Ok(source);
    }
    let references = git(cache, &["ls-remote", repository, "HEAD"])?;
    let commit = references
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let commit = normalize_commit(parts.next()?)?;
            (parts.next()? == "HEAD").then_some(commit)
        })
        .context("remote repository has no default-branch commit")?;
    // HEAD may equal an earlier candidate; retry it as the final fallback.
    let path = checkout(cache, repository, &commit)?;
    Ok(Source {
        path,
        repository: repository.to_owned(),
        commit,
        method: Method::DefaultBranch,
    })
}
