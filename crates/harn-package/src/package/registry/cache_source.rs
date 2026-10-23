//! Package source lifetimes and the cache locks that protect their bytes.

use crate::package::*;
use harn_modules::package_snapshot::{open_lock_file, PackageSnapshot};
use std::sync::Arc;

const CACHE_LOCK_TIMEOUT: Duration = Duration::from_mins(10);
const CACHE_LIFETIME_LOCK: &str = "cache.lock";

#[derive(Debug, Clone)]
pub(crate) struct PackageCache {
    root: PathBuf,
    _lease: Arc<File>,
    access: CacheAccess,
}

#[derive(Debug, Clone, Copy)]
enum CacheAccess {
    Inspect,
    Write,
}

impl PackageCache {
    pub(crate) fn open(workspace: &PackageWorkspace) -> Result<Self, PackageError> {
        Self::acquire(workspace, harn_flock::LockMode::Shared)
    }

    pub(crate) fn clean(workspace: &PackageWorkspace) -> Result<Self, PackageError> {
        Self::acquire(workspace, harn_flock::LockMode::Exclusive)
    }

    pub(crate) fn inspect(workspace: &PackageWorkspace) -> Result<Option<Self>, PackageError> {
        let root = workspace.cache_root()?;
        let Some(file) = open_existing_cache_lock(&root.join(CACHE_LIFETIME_LOCK))? else {
            if root.join("git").try_exists().map_err(|error| {
                format!(
                    "failed to inspect package cache {}: {error}",
                    root.display()
                )
            })? || root.join("archive").try_exists().map_err(|error| {
                format!(
                    "failed to inspect package cache {}: {error}",
                    root.display()
                )
            })? {
                return Err(format!(
                    "package cache lifetime lock is missing at {}",
                    root.display()
                )
                .into());
            }
            return Ok(None);
        };
        lock_open_cache_file(
            &file,
            &root.join(CACHE_LIFETIME_LOCK),
            harn_flock::LockMode::Shared,
        )?;
        Ok(Some(Self {
            root,
            _lease: Arc::new(file),
            access: CacheAccess::Inspect,
        }))
    }

    fn acquire(
        workspace: &PackageWorkspace,
        mode: harn_flock::LockMode,
    ) -> Result<Self, PackageError> {
        let root = workspace.cache_root()?;
        let file = lock_cache_file(&root.join(CACHE_LIFETIME_LOCK), mode)?;
        Ok(Self {
            root,
            _lease: Arc::new(file),
            access: CacheAccess::Write,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn git(&self, source: &str, commit: &str) -> Result<CacheEntry, PackageError> {
        self.entry(CacheKind::Git, &sha256_hex(source.as_bytes()), commit)
    }

    pub(crate) fn archive(
        &self,
        source: &str,
        content_hash: &str,
    ) -> Result<CacheEntry, PackageError> {
        self.entry(
            CacheKind::Archive,
            &sha256_hex(source.as_bytes()),
            archive_cache_key(content_hash)?,
        )
    }

    pub(crate) fn entry(
        &self,
        kind: CacheKind,
        source_hash: &str,
        key: &str,
    ) -> Result<CacheEntry, PackageError> {
        let path = cache_entry_lock_path(&self.root, source_hash, key);
        let lock = match self.access {
            CacheAccess::Write => lock_cache_file(&path, harn_flock::LockMode::Exclusive)?,
            CacheAccess::Inspect => {
                let file = File::open(&path).map_err(|error| {
                    format!(
                        "failed to open package cache lock {}: {error}",
                        path.display()
                    )
                })?;
                lock_open_cache_file(&file, &path, harn_flock::LockMode::Shared)?;
                file
            }
        };
        Ok(CacheEntry {
            directory: cache_entry_path(&self.root, kind, source_hash, key),
            _lock: lock,
            _cache: self.clone(),
        })
    }
}

fn lock_cache_file(path: &Path, mode: harn_flock::LockMode) -> Result<File, PackageError> {
    let file = open_lock_file(path).map_err(|error| PackageError::Registry(error.to_string()))?;
    lock_open_cache_file(&file, path, mode)?;
    Ok(file)
}

fn open_existing_cache_lock(path: &Path) -> Result<Option<File>, PackageError> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to open package cache lock {}: {error}",
            path.display()
        )
        .into()),
    }
}

fn lock_open_cache_file(
    file: &File,
    path: &Path,
    mode: harn_flock::LockMode,
) -> Result<(), PackageError> {
    harn_flock::lock_with_deadline(file, path, mode, CACHE_LOCK_TIMEOUT)
        .map_err(|error| PackageError::Registry(error.to_string()))?;
    Ok(())
}

#[derive(Debug)]
pub(crate) struct CacheEntry {
    directory: PathBuf,
    _lock: File,
    _cache: PackageCache,
}

impl CacheEntry {
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(super) fn verified(self, content_hash: String) -> CachedPackage {
        CachedPackage {
            entry: self,
            content_hash,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CachedPackage {
    entry: CacheEntry,
    content_hash: String,
}

impl CachedPackage {
    pub(crate) fn content_hash(&self) -> &str {
        &self.content_hash
    }

    pub(crate) fn directory(&self) -> &Path {
        self.entry.directory()
    }
}

enum SourceLifetime {
    Local,
    Cached { _package: CachedPackage },
    Installed { _snapshot: Arc<PackageSnapshot> },
}

pub(crate) struct PackageSource {
    directory: PathBuf,
    _lifetime: SourceLifetime,
}

impl PackageSource {
    pub(crate) fn local(directory: PathBuf) -> Self {
        Self {
            _lifetime: SourceLifetime::Local,
            directory,
        }
    }

    pub(crate) fn installed(
        snapshot: Arc<PackageSnapshot>,
        alias: &str,
        expected_hash: &str,
    ) -> Result<Option<Self>, PackageError> {
        validate_package_alias(alias)?;
        let directory = snapshot.packages_root().join(alias);
        Ok(
            (directory.is_dir() && materialized_hash_matches(&directory, expected_hash)).then_some(
                Self {
                    directory,
                    _lifetime: SourceLifetime::Installed {
                        _snapshot: snapshot,
                    },
                },
            ),
        )
    }
}

impl AsRef<Path> for PackageSource {
    fn as_ref(&self) -> &Path {
        &self.directory
    }
}

impl From<CachedPackage> for PackageSource {
    fn from(cached: CachedPackage) -> Self {
        Self {
            directory: cached.directory().to_path_buf(),
            _lifetime: SourceLifetime::Cached { _package: cached },
        }
    }
}
