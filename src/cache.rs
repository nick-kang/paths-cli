use std::{
    collections::HashSet,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, ensure};

use crate::source;

pub const MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Default)]
pub struct Usage {
    created: bool,
    recording_failed: bool,
    protected: HashSet<PathBuf>,
}

impl Usage {
    pub fn record(&mut self, source: &source::Source) {
        self.created |= source.created;
        self.recording_failed |= !source.usage_recorded;
        self.protected.insert(source.path.clone());
    }

    pub fn cleanup(&self, cache: &Path, budget: u64) {
        if self.created && !self.recording_failed {
            match prune(cache, budget, &self.protected) {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "warning: cache remains above its size limit; some checkouts could not be removed"
                ),
                Err(error) => eprintln!("warning: cache cleanup failed: {error:#}"),
            }
        }
    }
}

fn marker(path: &Path) -> PathBuf {
    path.with_extension("last-used")
}

pub fn record_use(path: &Path) -> bool {
    let result = (|| -> Result<()> {
        let marker = marker(path);
        if let Ok(metadata) = fs::symlink_metadata(&marker) {
            ensure!(metadata.is_file(), "unexpected usage marker");
        }
        File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(marker)?
            .set_modified(SystemTime::now())?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!(
            "warning: unable to record cache usage for {}: {error:#}",
            path.display()
        );
        return false;
    }
    true
}

fn last_used(path: &Path) -> Result<Option<SystemTime>> {
    match fs::symlink_metadata(marker(path)) {
        Ok(metadata) => {
            ensure!(metadata.is_file(), "unexpected usage marker");
            Ok(Some(metadata.modified()?))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn prune(cache: &Path, budget: u64, protected: &HashSet<PathBuf>) -> Result<bool> {
    let root = cache.join("v1");
    if !root.try_exists()? {
        return Ok(true);
    }
    ensure!(
        fs::symlink_metadata(&root)?.is_dir(),
        "unexpected cache root"
    );
    let mut candidates = Vec::new();
    let mut total = 0_u64;
    for repository in fs::read_dir(root)? {
        let repository = repository?;
        if !repository.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(repository.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            let size = fs_extra::dir::get_size(&path)?;
            total = total.checked_add(size).context("cache size overflow")?;
            let name = entry.file_name();
            let Some(commit) = name.to_str() else {
                continue;
            };
            if source::normalize_commit(commit).as_deref() != Some(commit)
                || protected.contains(&path)
            {
                continue;
            }
            match last_used(&path) {
                Ok(used) => candidates.push((used, path, size)),
                Err(error) => eprintln!(
                    "warning: skipping cache entry {}: {error:#}",
                    path.display()
                ),
            }
        }
    }
    candidates.sort();
    // ponytail: scan on growth; persist size metadata if large caches make this costly.
    for (used, path, size) in candidates {
        if total <= budget {
            break;
        }
        match evict(cache, &path, used) {
            Ok(true) => total = total.saturating_sub(size),
            Ok(false) => {}
            Err(error) => eprintln!(
                "warning: skipping cache entry {}: {error:#}",
                path.display()
            ),
        }
    }
    Ok(total <= budget)
}

fn evict(cache: &Path, path: &Path, used: Option<SystemTime>) -> Result<bool> {
    let parent = path.parent().context("checkout has no parent")?;
    ensure!(
        fs::symlink_metadata(parent)?.is_dir(),
        "unexpected repository directory"
    );
    let lock_path = parent.join(".lock");
    ensure!(
        fs::symlink_metadata(&lock_path)?.is_file(),
        "unexpected repository lock"
    );
    let lock = File::options().read(true).write(true).open(lock_path)?;
    if let Err(error) = lock.try_lock() {
        return match error {
            std::fs::TryLockError::WouldBlock => Ok(false),
            std::fs::TryLockError::Error(error) => Err(error.into()),
        };
    }
    if !path.try_exists()? {
        return Ok(false);
    }
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "unexpected checkout directory"
    );
    ensure!(
        fs::symlink_metadata(path.join(".git"))?.is_dir(),
        "unexpected Git directory"
    );
    if last_used(path)? != used {
        return Ok(false);
    }
    let repository = source::git(path, &["config", "--local", "--get", "remote.origin.url"])?;
    ensure!(
        source::cache_repository(cache, &repository) == parent,
        "cache identity mismatch"
    );
    let commit = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid commit directory")?;
    source::current_checkout(path, &repository, commit)?;
    if !source::git(
        path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?
    .is_empty()
    {
        return Ok(false);
    }
    fs::remove_dir_all(path)?;
    match fs::remove_file(marker(path)) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => eprintln!("warning: unable to remove usage marker: {error}"),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_rechecks_usage_before_git_or_deletion() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let parent = cache.path().join("v1/repo");
        let path = parent.join("checkout");
        fs::create_dir_all(path.join(".git"))?;
        File::create(parent.join(".lock"))?;
        assert!(record_use(&path));
        // The scan saw an unmarked entry; another invocation has since used it.
        assert!(!evict(cache.path(), &path, None)?);
        assert!(path.exists());
        Ok(())
    }
}
