//! What a cached or materialized package tree contains: the recursive
//! content hash, the copy/symlink materialization of a path dependency, and
//! the checks that a tree still matches its recorded hash.

use crate::package::*;

pub(crate) fn normalized_relative_path(path: &Path) -> String {
    harn_modules::package_execution::normalized_package_relative_path(path)
}

pub(crate) fn compute_content_hash(dir: &Path) -> Result<String, PackageError> {
    harn_modules::package_execution::compute_package_content_hash(dir)
        .map_err(|error| PackageError::Registry(error.to_string()))
}

pub(crate) fn verify_content_hash_or_compute(
    dir: &Path,
    expected: &str,
) -> Result<(), PackageError> {
    let actual = compute_content_hash(dir)?;
    if actual != expected {
        return Err(format!(
            "content hash mismatch for {}: expected {}, got {}",
            dir.display(),
            expected,
            actual
        )
        .into());
    }
    if read_cached_content_hash(dir)?.as_deref() != Some(expected) {
        write_cached_content_hash(dir, expected)?;
    }
    Ok(())
}

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), PackageError> {
    fs::create_dir_all(dst)
        .map_err(|error| format!("failed to create {}: {error}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|error| format!("failed to read {}: {error}", src.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", src.display()))?;
        let ty = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", entry.path().display()))?;
        let name = entry.file_name();
        if name == OsStr::new(".git")
            || name == OsStr::new(CONTENT_HASH_FILE)
            || name == OsStr::new(CACHE_METADATA_FILE)
        {
            continue;
        }
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if ty.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            fs::copy(entry.path(), &dest_path).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    entry.path().display(),
                    dest_path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), PackageError> {
    std::os::unix::fs::symlink(source, dest).map_err(|error| {
        PackageError::Registry(format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        ))
    })
}

#[cfg(windows)]
pub(crate) fn symlink_path_dependency(source: &Path, dest: &Path) -> Result<(), PackageError> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, dest)
    } else {
        std::os::windows::fs::symlink_file(source, dest)
    }
    .map_err(|error| {
        PackageError::Registry(format!(
            "failed to symlink {} to {}: {error}",
            source.display(),
            dest.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn symlink_path_dependency(_source: &Path, _dest: &Path) -> Result<(), PackageError> {
    Err("symlinks are not supported on this platform"
        .to_string()
        .into())
}

pub(crate) fn materialize_path_dependency(
    source: &Path,
    dest_root: &Path,
    alias: &str,
) -> Result<(), PackageError> {
    if source.is_dir() {
        let dest = dest_root.join(alias);
        match symlink_path_dependency(source, &dest) {
            Ok(()) => Ok(()),
            Err(_) => copy_dir_recursive(source, &dest),
        }
    } else {
        let dest = dest_root.join(format!("{alias}.harn"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        match symlink_path_dependency(source, &dest) {
            Ok(()) => Ok(()),
            Err(_) => {
                fs::copy(source, &dest).map_err(|error| {
                    format!(
                        "failed to copy {} to {}: {error}",
                        source.display(),
                        dest.display()
                    )
                })?;
                Ok(())
            }
        }
    }
}

pub(crate) fn materialized_hash_matches(dir: &Path, expected: &str) -> bool {
    verify_content_hash_or_compute(dir, expected).is_ok()
}

pub(crate) fn resolve_path_dependency_source(
    manifest_dir: &Path,
    raw: &str,
) -> Result<PathBuf, PackageError> {
    let source = {
        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            manifest_dir.join(candidate)
        }
    };
    if source.exists() {
        return source.canonicalize().map_err(|error| {
            PackageError::Registry(format!(
                "failed to canonicalize {}: {error}",
                source.display()
            ))
        });
    }
    if source.extension().is_none() {
        let with_ext = source.with_extension("harn");
        if with_ext.exists() {
            return with_ext.canonicalize().map_err(|error| {
                PackageError::Registry(format!(
                    "failed to canonicalize {}: {error}",
                    with_ext.display()
                ))
            });
        }
    }
    Err(format!("package source not found: {}", source.display()).into())
}
