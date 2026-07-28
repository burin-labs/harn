//! Filling a cache directory from its source: cloning a git commit into the
//! git cache, and unpacking a downloaded archive into the archive cache.

use crate::package::*;

pub(crate) fn ensure_git_cache_populated_in(
    workspace: &PackageWorkspace,
    url: &str,
    source: &str,
    commit: &str,
    expected_hash: Option<&str>,
    refetch: bool,
    offline: bool,
) -> Result<String, PackageError> {
    let cache_dir = git_cache_dir_in(workspace, source, commit)?;
    let _lock = acquire_git_cache_lock_in(workspace, source, commit)?;
    if refetch && cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)
            .map_err(|error| format!("failed to remove {}: {error}", cache_dir.display()))?;
    }
    if cache_dir.exists() {
        if let Some(expected) = expected_hash {
            verify_content_hash_or_compute(&cache_dir, expected)?;
            write_cache_metadata(&cache_dir, source, commit, expected)?;
            return Ok(expected.to_string());
        }
        let hash = compute_content_hash(&cache_dir)?;
        write_cached_content_hash(&cache_dir, &hash)?;
        write_cache_metadata(&cache_dir, source, commit, &hash)?;
        return Ok(hash);
    }

    if offline {
        return Err(format!(
            "package cache entry for {source} at {commit} is missing; cannot fetch in offline mode"
        )
        .into());
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| format!("invalid cache path {}", cache_dir.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temp_dir = unique_temp_dir(parent, "tmp")?;
    let populated = (|| -> Result<String, PackageError> {
        clone_git_commit_to(url, commit, &temp_dir)?;
        let hash = compute_content_hash(&temp_dir)?;
        if let Some(expected) = expected_hash {
            if hash != expected {
                return Err(format!(
                    "content hash mismatch for {source} at {commit}: expected {expected}, got {hash}"
                )
                .into());
            }
        }
        write_cached_content_hash(&temp_dir, &hash)?;
        write_cache_metadata(&temp_dir, source, commit, &hash)?;
        fs::rename(&temp_dir, &cache_dir).map_err(|error| {
            format!(
                "failed to move {} to {}: {error}",
                temp_dir.display(),
                cache_dir.display()
            )
        })?;
        Ok(hash)
    })();
    let hash = match populated {
        Ok(hash) => hash,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
    };
    Ok(hash)
}

fn archive_entry_relative_path(path: &Path) -> Result<PathBuf, PackageError> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => out.push(value),
            std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "package archive entry must be relative and contained within the package root: {}",
                    path.display()
                )
                .into());
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err("package archive entry path cannot be empty"
            .to_string()
            .into());
    }
    Ok(out)
}

pub(crate) fn unpack_package_archive_bytes(bytes: &[u8], dest: &Path) -> Result<(), PackageError> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("failed to read package archive: {error}"))?;
    let mut unpacked_bytes = 0u64;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("failed to read package archive entry: {error}"))?;
        let entry_type = entry.header().entry_type();
        let raw_path = entry
            .path()
            .map_err(|error| format!("failed to read package archive entry path: {error}"))?;
        let relative = archive_entry_relative_path(&raw_path)?;
        let target = dest.join(relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("failed to create {}: {error}", target.display()))?;
        } else if entry_type.is_file() {
            unpacked_bytes = unpacked_bytes
                .checked_add(entry.size())
                .ok_or_else(|| "package archive expanded size overflowed".to_string())?;
            if unpacked_bytes > PACKAGE_ARCHIVE_MAX_UNPACKED_BYTES {
                return Err(format!(
                    "package archive expands above the {PACKAGE_ARCHIVE_MAX_UNPACKED_BYTES} byte limit"
                )
                .into());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            entry
                .unpack(&target)
                .map_err(|error| format!("failed to unpack {}: {error}", target.display()))?;
        } else {
            return Err(format!(
                "package archive entry {} has unsupported type {:?}",
                raw_path.display(),
                entry_type
            )
            .into());
        }
    }
    if !dest.join(MANIFEST).is_file() {
        return Err(format!("package archive is missing {MANIFEST} at its root").into());
    }
    Ok(())
}

pub(crate) fn ensure_archive_cache_populated_in(
    workspace: &PackageWorkspace,
    url: &str,
    source: &str,
    expected_hash: &str,
    refetch: bool,
    offline: bool,
) -> Result<String, PackageError> {
    archive_cache_key(expected_hash)?;
    let cache_dir = archive_cache_dir_in(workspace, source, expected_hash)?;
    let _lock = acquire_archive_cache_lock_in(workspace, source, expected_hash)?;
    if refetch && cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)
            .map_err(|error| format!("failed to remove {}: {error}", cache_dir.display()))?;
    }
    if cache_dir.exists() {
        verify_content_hash_or_compute(&cache_dir, expected_hash)?;
        write_cache_metadata(&cache_dir, source, expected_hash, expected_hash)?;
        return Ok(expected_hash.to_string());
    }
    if offline {
        return Err(format!(
            "package cache entry for {source} at {expected_hash} is missing; cannot fetch in offline mode"
        )
        .into());
    }

    let parent = cache_dir
        .parent()
        .ok_or_else(|| format!("invalid cache path {}", cache_dir.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temp_dir = unique_temp_dir(parent, "tmp")?;
    let populated = (|| -> Result<String, PackageError> {
        fs::create_dir_all(&temp_dir)
            .map_err(|error| format!("failed to create {}: {error}", temp_dir.display()))?;
        let bytes = read_package_archive_bytes(url)?;
        unpack_package_archive_bytes(&bytes, &temp_dir)?;
        verify_content_hash_or_compute(&temp_dir, expected_hash)?;
        write_cached_content_hash(&temp_dir, expected_hash)?;
        write_cache_metadata(&temp_dir, source, expected_hash, expected_hash)?;
        fs::rename(&temp_dir, &cache_dir).map_err(|error| {
            format!(
                "failed to move {} to {}: {error}",
                temp_dir.display(),
                cache_dir.display()
            )
        })?;
        Ok(expected_hash.to_string())
    })();
    match populated {
        Ok(hash) => Ok(hash),
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            Err(error)
        }
    }
}
