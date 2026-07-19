use std::path::{Component, Path, PathBuf};

use super::FsAccess;

pub(super) fn normalize_for_policy(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    let absolute = normalize_lexically(&absolute);
    if let Ok(canonical) = absolute.canonicalize() {
        return normalize_os_path_alias(&canonical);
    }

    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return normalize_os_path_alias(&normalize_lexically(&absolute));
        };
        if let Some(name) = existing.file_name() {
            suffix.push(name.to_os_string());
        }
        existing = parent;
    }

    let mut normalized = existing
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(existing));
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    normalize_os_path_alias(&normalize_lexically(&normalized))
}

#[cfg(target_os = "macos")]
fn normalize_os_path_alias(path: &Path) -> PathBuf {
    // macOS exposes `/tmp` and `/var` as aliases of `/private/tmp` and
    // `/private/var`, while sandbox-exec evaluates subpaths against the
    // canonical names. Keep policy roots in that canonical namespace so a
    // broad writable alias cannot bypass a narrower read-only rule.
    for alias in [Path::new("/tmp"), Path::new("/var")] {
        if let Ok(relative) = path.strip_prefix(alias) {
            return Path::new("/private")
                .join(alias.strip_prefix("/").unwrap())
                .join(relative);
        }
    }
    path.to_path_buf()
}

#[cfg(not(target_os = "macos"))]
fn normalize_os_path_alias(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub(super) fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// Resolve `path` without canonicalizing it so `/dev/stdout` remains a stable
/// standard-device name on macOS rather than becoming a per-process `/dev/fd`
/// alias.
pub(super) fn normalize_io_device_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    normalize_lexically(&absolute)
}

/// Whether `path` is one of the standard process I/O device files that the
/// sandbox treats as a stream rather than a workspace mutation for this access.
pub(super) fn is_standard_io_device_for_access(path: &Path, access: FsAccess) -> bool {
    match access {
        FsAccess::Read => {
            matches!(
                path.to_str(),
                Some("/dev/stdin" | "/dev/stdout" | "/dev/stderr" | "/dev/null")
            ) || is_dev_fd_descriptor(path)
        }
        FsAccess::Write => {
            matches!(
                path.to_str(),
                Some("/dev/stdout" | "/dev/stderr" | "/dev/null")
            ) || is_dev_fd_descriptor(path)
        }
        FsAccess::Delete => false,
    }
}

fn is_dev_fd_descriptor(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let Some(fd) = text.strip_prefix("/dev/fd/") else {
        return false;
    };
    !fd.is_empty() && fd.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(target_os = "macos")]
#[cfg(test)]
mod tests {
    use super::normalize_for_policy;
    use std::path::{Path, PathBuf};

    #[test]
    fn policy_paths_use_sandbox_exec_canonical_aliases() {
        assert_eq!(
            normalize_for_policy(Path::new("/tmp")),
            PathBuf::from("/private/tmp")
        );
        assert_eq!(
            normalize_for_policy(Path::new("/var/folders")),
            PathBuf::from("/private/var/folders")
        );
    }
}
