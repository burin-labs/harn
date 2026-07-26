//! Where a fetched package lives on disk and how concurrent installs
//! serialize on it: cache roots, per-source cache directories, their lock
//! files, and the metadata/content-hash markers written beside them.

use crate::package::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PackageCacheMetadata {
    pub(super) version: u32,
    pub(super) source: String,
    pub(super) commit: String,
    pub(super) content_hash: String,
    pub(super) cached_at_unix_ms: u128,
}

pub(crate) fn cache_root() -> Result<PathBuf, PackageError> {
    PackageWorkspace::from_current_dir()?.cache_root()
}

pub(crate) fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

pub(crate) fn git_cache_dir_in(
    workspace: &PackageWorkspace,
    source: &str,
    commit: &str,
) -> Result<PathBuf, PackageError> {
    Ok(workspace
        .cache_root()?
        .join("git")
        .join(sha256_hex(source))
        .join(commit))
}

pub(crate) fn git_cache_lock_path_in(
    workspace: &PackageWorkspace,
    source: &str,
    commit: &str,
) -> Result<PathBuf, PackageError> {
    Ok(workspace
        .cache_root()?
        .join("locks")
        .join(format!("{}-{commit}.lock", sha256_hex(source))))
}

pub(crate) fn archive_cache_key(content_hash: &str) -> Result<&str, PackageError> {
    let Some(value) = content_hash.strip_prefix("sha256:") else {
        return Err(format!("archive checksum must use sha256:<hex>, got {content_hash}").into());
    };
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            format!("archive checksum must use sha256:<64 hex>, got {content_hash}").into(),
        );
    }
    Ok(value)
}

pub(crate) fn archive_cache_dir_in(
    workspace: &PackageWorkspace,
    source: &str,
    content_hash: &str,
) -> Result<PathBuf, PackageError> {
    Ok(workspace
        .cache_root()?
        .join("archive")
        .join(sha256_hex(source))
        .join(archive_cache_key(content_hash)?))
}

pub(crate) fn archive_cache_lock_path_in(
    workspace: &PackageWorkspace,
    source: &str,
    content_hash: &str,
) -> Result<PathBuf, PackageError> {
    Ok(workspace.cache_root()?.join("locks").join(format!(
        "{}-{}.lock",
        sha256_hex(source),
        archive_cache_key(content_hash)?
    )))
}

pub(crate) fn acquire_git_cache_lock_in(
    workspace: &PackageWorkspace,
    source: &str,
    commit: &str,
) -> Result<File, PackageError> {
    let path = git_cache_lock_path_in(workspace, source, commit)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let file = File::create(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    file.lock()
        .map_err(|error| format!("failed to lock {}: {error}", path.display()))?;
    Ok(file)
}

pub(crate) fn acquire_archive_cache_lock_in(
    workspace: &PackageWorkspace,
    source: &str,
    content_hash: &str,
) -> Result<File, PackageError> {
    let path = archive_cache_lock_path_in(workspace, source, content_hash)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let file = File::create(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    file.lock()
        .map_err(|error| format!("failed to lock {}: {error}", path.display()))?;
    Ok(file)
}

pub(crate) fn read_cached_content_hash(dir: &Path) -> Result<Option<String>, PackageError> {
    let path = dir.join(CONTENT_HASH_FILE);
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to read {}: {error}", path.display()).into()),
    }
}

pub(crate) fn write_cached_content_hash(dir: &Path, hash: &str) -> Result<(), PackageError> {
    let path = dir.join(CONTENT_HASH_FILE);
    harn_vm::atomic_io::atomic_write(&path, format!("{hash}\n").as_bytes()).map_err(|error| {
        PackageError::Registry(format!("failed to write {}: {error}", path.display()))
    })
}

pub(crate) fn read_cache_metadata(
    dir: &Path,
) -> Result<Option<PackageCacheMetadata>, PackageError> {
    let path = dir.join(CACHE_METADATA_FILE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display()).into()),
    };
    let metadata = toml::from_str::<PackageCacheMetadata>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if metadata.version != CACHE_METADATA_VERSION {
        return Err(format!(
            "unsupported {} version {} (expected {})",
            path.display(),
            metadata.version,
            CACHE_METADATA_VERSION
        )
        .into());
    }
    Ok(Some(metadata))
}

pub(crate) fn write_cache_metadata(
    dir: &Path,
    source: &str,
    commit: &str,
    content_hash: &str,
) -> Result<(), PackageError> {
    let cached_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_millis();
    let metadata = PackageCacheMetadata {
        version: CACHE_METADATA_VERSION,
        source: source.to_string(),
        commit: commit.to_string(),
        content_hash: content_hash.to_string(),
        cached_at_unix_ms,
    };
    let body = toml::to_string_pretty(&metadata)
        .map_err(|error| format!("failed to encode cache metadata: {error}"))?;
    let path = dir.join(CACHE_METADATA_FILE);
    harn_vm::atomic_io::atomic_write(&path, body.as_bytes()).map_err(|error| {
        PackageError::Registry(format!("failed to write {}: {error}", path.display()))
    })
}
