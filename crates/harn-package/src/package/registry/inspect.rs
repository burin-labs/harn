//! Reading the cache back: enumerating what is cached, verifying entries
//! against their lock and recorded hashes, and pruning them.

use crate::package::*;

#[derive(Debug, Clone)]
pub(crate) struct PackageCacheEntry {
    pub(super) path: PathBuf,
    pub(super) kind: &'static str,
    pub(super) source_hash: String,
    pub(super) commit: String,
    pub(super) metadata: Option<PackageCacheMetadata>,
}

pub(crate) fn discover_package_cache_entries() -> Result<Vec<PackageCacheEntry>, PackageError> {
    discover_package_cache_entries_in(&PackageWorkspace::from_current_dir()?)
}

pub(crate) fn discover_package_cache_entries_in(
    workspace: &PackageWorkspace,
) -> Result<Vec<PackageCacheEntry>, PackageError> {
    let mut entries = discover_cache_entries_for_kind(workspace, "git")?;
    entries.extend(discover_cache_entries_for_kind(workspace, "archive")?);
    entries.sort_by(|left, right| {
        left.kind
            .cmp(right.kind)
            .then_with(|| left.source_hash.cmp(&right.source_hash))
            .then_with(|| left.commit.cmp(&right.commit))
    });
    Ok(entries)
}

fn discover_cache_entries_for_kind(
    workspace: &PackageWorkspace,
    kind: &'static str,
) -> Result<Vec<PackageCacheEntry>, PackageError> {
    let root = workspace.cache_root()?.join(kind);
    let mut entries = Vec::new();
    let source_dirs = match fs::read_dir(&root) {
        Ok(source_dirs) => source_dirs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(format!("failed to read {}: {error}", root.display()).into()),
    };
    for source_dir in source_dirs {
        let source_dir = source_dir
            .map_err(|error| format!("failed to read {} entry: {error}", root.display()))?;
        let source_type = source_dir
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", source_dir.path().display()))?;
        if !source_type.is_dir() {
            continue;
        }
        let source_hash = source_dir.file_name().to_string_lossy().to_string();
        let commit_dirs = fs::read_dir(source_dir.path())
            .map_err(|error| format!("failed to read {}: {error}", source_dir.path().display()))?;
        for commit_dir in commit_dirs {
            let commit_dir = commit_dir.map_err(|error| {
                format!(
                    "failed to read {} entry: {error}",
                    source_dir.path().display()
                )
            })?;
            let commit_type = commit_dir.file_type().map_err(|error| {
                format!("failed to stat {}: {error}", commit_dir.path().display())
            })?;
            if !commit_type.is_dir() {
                continue;
            }
            let commit = commit_dir.file_name().to_string_lossy().to_string();
            if commit.starts_with("tmp-") || commit.ends_with(".full-clone") {
                continue;
            }
            let metadata = read_cache_metadata(&commit_dir.path())?;
            entries.push(PackageCacheEntry {
                path: commit_dir.path(),
                kind,
                source_hash: source_hash.clone(),
                commit,
                metadata,
            });
        }
    }
    entries.sort_by(|left, right| {
        left.source_hash
            .cmp(&right.source_hash)
            .then_with(|| left.commit.cmp(&right.commit))
    });
    Ok(entries)
}

pub(crate) fn locked_package_cache_paths_in(
    workspace: &PackageWorkspace,
    lock: &LockFile,
) -> Result<HashSet<PathBuf>, PackageError> {
    let mut keep = HashSet::new();
    for entry in &lock.packages {
        validate_package_alias(&entry.name)?;
        if entry.source.starts_with("git+") {
            let commit = entry
                .commit
                .as_deref()
                .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
            keep.insert(git_cache_dir_in(workspace, &entry.source, commit)?);
        } else if entry.source.starts_with("archive+") {
            let expected_hash = entry
                .content_hash
                .as_deref()
                .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
            keep.insert(archive_cache_dir_in(
                workspace,
                &entry.source,
                expected_hash,
            )?);
        }
    }
    Ok(keep)
}

pub(crate) fn verify_lock_entry_cache_in(
    workspace: &PackageWorkspace,
    entry: &LockEntry,
) -> Result<bool, PackageError> {
    validate_package_alias(&entry.name)?;
    if entry.source.starts_with("path+") {
        let path = path_from_source_uri(&entry.source)?;
        if !path.exists() {
            return Err(format!(
                "path dependency {} source is missing: {}",
                entry.name,
                path.display()
            )
            .into());
        }
        return Ok(false);
    }
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    let (cache_dir, cache_key) = if entry.source.starts_with("git+") {
        let commit = entry
            .commit
            .as_deref()
            .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
        (git_cache_dir_in(workspace, &entry.source, commit)?, commit)
    } else if entry.source.starts_with("archive+") {
        (
            archive_cache_dir_in(workspace, &entry.source, expected_hash)?,
            expected_hash,
        )
    } else {
        return Ok(false);
    };
    if !cache_dir.is_dir() {
        return Err(format!(
            "package cache entry for {} is missing: {}",
            entry.name,
            cache_dir.display()
        )
        .into());
    }
    verify_content_hash_or_compute(&cache_dir, expected_hash)?;
    match read_cache_metadata(&cache_dir)? {
        Some(metadata)
            if metadata.source == entry.source
                && metadata.commit == cache_key
                && metadata.content_hash == expected_hash => {}
        Some(metadata) => {
            return Err(format!(
                "package cache metadata mismatch for {}: expected {} {} {}, got {} {} {}",
                entry.name,
                entry.source,
                cache_key,
                expected_hash,
                metadata.source,
                metadata.commit,
                metadata.content_hash
            )
            .into());
        }
        None => write_cache_metadata(&cache_dir, &entry.source, cache_key, expected_hash)?,
    }
    Ok(true)
}

pub(crate) fn verify_materialized_lock_entry(
    packages_dir: &Path,
    entry: &LockEntry,
) -> Result<bool, PackageError> {
    validate_package_alias(&entry.name)?;
    if entry.source.starts_with("path+") {
        let dir = packages_dir.join(&entry.name);
        let file = packages_dir.join(format!("{}.harn", entry.name));
        if !dir.exists() && !file.exists() {
            return Err(format!(
                "materialized path dependency {} is missing under {}",
                entry.name,
                packages_dir.display()
            )
            .into());
        }
        return Ok(true);
    }
    if !entry.source.starts_with("git+") && !entry.source.starts_with("archive+") {
        return Ok(false);
    }
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    let dest_dir = packages_dir.join(&entry.name);
    if !dest_dir.is_dir() {
        return Err(format!(
            "materialized package {} is missing: {}",
            entry.name,
            dest_dir.display()
        )
        .into());
    }
    verify_content_hash_or_compute(&dest_dir, expected_hash)?;
    Ok(true)
}

pub(crate) fn verify_package_cache_impl(materialized: bool) -> Result<usize, PackageError> {
    verify_package_cache_in(&PackageWorkspace::from_current_dir()?, materialized)
}

pub(crate) fn verify_package_cache_in(
    workspace: &PackageWorkspace,
    materialized: bool,
) -> Result<usize, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?
        .ok_or_else(|| format!("{} is missing", ctx.lock_path().display()))?;
    validate_lock_matches_manifest(workspace, &ctx, &lock)?;
    let snapshot = materialized
        .then(|| current_package_snapshot(&ctx))
        .transpose()?;
    let mut verified = 0usize;
    for entry in &lock.packages {
        if verify_lock_entry_cache_in(workspace, entry)? {
            verified += 1;
        }
        if let Some(snapshot) = snapshot.as_ref() {
            if verify_materialized_lock_entry(snapshot.packages_root(), entry)? {
                verified += 1;
            }
        }
    }
    Ok(verified)
}

pub(crate) fn clean_package_cache_impl(all: bool) -> Result<usize, PackageError> {
    clean_package_cache_in(&PackageWorkspace::from_current_dir()?, all)
}

pub(crate) fn clean_package_cache_in(
    workspace: &PackageWorkspace,
    all: bool,
) -> Result<usize, PackageError> {
    let entries = discover_package_cache_entries_in(workspace)?;
    if entries.is_empty() {
        return Ok(0);
    }
    if all {
        let root = workspace.cache_root()?;
        for child in ["git", "archive", "locks"] {
            let path = root.join(child);
            if path.exists() {
                fs::remove_dir_all(&path)
                    .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
            }
        }
        return Ok(entries.len());
    }

    let ctx = workspace.load_manifest_context()?;
    let lock = LockFile::load(&ctx.lock_path())?
        .ok_or_else(|| format!("{LOCK_FILE} is missing; pass --all to clean every cache entry"))?;
    validate_lock_matches_manifest(workspace, &ctx, &lock)?;
    let keep = locked_package_cache_paths_in(workspace, &lock)?;
    let mut removed = 0usize;
    for entry in entries {
        if keep.contains(&entry.path) {
            continue;
        }
        fs::remove_dir_all(&entry.path)
            .map_err(|error| format!("failed to remove {}: {error}", entry.path.display()))?;
        removed += 1;
        if let Some(parent) = entry.path.parent() {
            let is_empty = fs::read_dir(parent)
                .map(|mut children| children.next().is_none())
                .unwrap_or(false);
            if is_empty {
                fs::remove_dir(parent)
                    .map_err(|error| format!("failed to remove {}: {error}", parent.display()))?;
            }
        }
    }
    Ok(removed)
}
