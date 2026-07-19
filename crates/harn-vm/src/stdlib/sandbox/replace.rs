//! Symlink-safe atomic and conditional replacement inside sandbox scopes.

use super::*;

pub(crate) fn atomic_write_scoped_at_open(
    builtin: &str,
    path: &Path,
    contents: &[u8],
) -> io::Result<()> {
    atomic_replace_scoped_at_open(
        builtin,
        path,
        contents,
        crate::atomic_io::AtomicWriteDurability::Flush,
        true,
    )
    .map(|_| ())
}

pub(crate) fn atomic_replace_scoped_at_open(
    builtin: &str,
    path: &Path,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        crate::conditional_replace::require_parent(path, create_parents)?;
        return crate::atomic_io::atomic_write_with_durability(path, contents, durability);
    };
    atomic_replace_scoped_target(&target, contents, durability, create_parents)
}

/// Replace while the caller holds the canonical destination lock.
pub(crate) fn atomic_replace_scoped_at_open_unlocked(
    builtin: &str,
    path: &Path,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        crate::conditional_replace::require_parent(path, create_parents)?;
        return crate::atomic_io::atomic_write_with_durability_unlocked(path, contents, durability);
    };
    atomic_replace_scoped_target_unlocked(&target, contents, durability, create_parents)
}

pub(crate) fn read_for_replace_scoped_at_open(builtin: &str, path: &Path) -> io::Result<Vec<u8>> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return std::fs::read(path);
    };
    read_for_replace_scoped_target(&target)
}

#[cfg(unix)]
fn scoped_tmp_name(path: &Path) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    format!(".{file_name}.harn-tmp.{}.{counter}", std::process::id())
}

#[cfg(unix)]
pub(super) fn atomic_replace_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    use std::os::fd::AsRawFd;

    let (parent, file_name) = if create_parents {
        ensure_parent_dirs_scoped(target)?
    } else {
        open_parent_dir_scoped(target)?
    };
    let tmp_name = scoped_tmp_name(Path::new(&file_name));
    let mut file = openat_file(
        parent.as_raw_fd(),
        &tmp_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    let write_result = (|| -> io::Result<()> {
        if let Ok(existing) = openat_file(
            parent.as_raw_fd(),
            &file_name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) {
            if let Ok(metadata) = existing.metadata() {
                file.set_permissions(metadata.permissions())?;
            }
        }
        file.write_all(contents)?;
        file.flush()?;
        if durability == crate::atomic_io::AtomicWriteDurability::Flush {
            file.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = unlinkat_name(parent.as_raw_fd(), &tmp_name, 0);
        return Err(error);
    }
    if let Err(error) = renameat_name(
        parent.as_raw_fd(),
        &tmp_name,
        parent.as_raw_fd(),
        &file_name,
    ) {
        let _ = unlinkat_name(parent.as_raw_fd(), &tmp_name, 0);
        return Err(error);
    }
    let namespace_synced = durability == crate::atomic_io::AtomicWriteDurability::Flush
        && sync_dir_fd(parent.as_raw_fd());
    Ok(crate::atomic_io::AtomicWriteReceipt {
        file_synced: durability == crate::atomic_io::AtomicWriteDurability::Flush,
        namespace_synced,
    })
}

#[cfg(unix)]
fn read_for_replace_scoped_target(target: &ScopedMutationTarget) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd;

    let (parent, file_name) = open_parent_dir_scoped(target)?;
    let mut file = openat_file(
        parent.as_raw_fd(),
        &file_name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(windows)]
pub(super) fn atomic_replace_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    let (parent, file_name) = win_scoped_parent(target, create_parents)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    crate::atomic_io::atomic_write_with_durability(&full, contents, durability)
}

#[cfg(windows)]
fn atomic_replace_scoped_target_unlocked(
    target: &ScopedMutationTarget,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    let (parent, file_name) = win_scoped_parent(target, create_parents)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    crate::atomic_io::atomic_write_with_durability_unlocked(&full, contents, durability)
}

#[cfg(not(windows))]
fn atomic_replace_scoped_target_unlocked(
    target: &ScopedMutationTarget,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    atomic_replace_scoped_target(target, contents, durability, create_parents)
}

#[cfg(windows)]
fn read_for_replace_scoped_target(target: &ScopedMutationTarget) -> io::Result<Vec<u8>> {
    let (parent, file_name) = win_scoped_parent(target, false)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    std::fs::read(full)
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn atomic_replace_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
    durability: crate::atomic_io::AtomicWriteDurability,
    create_parents: bool,
) -> io::Result<crate::atomic_io::AtomicWriteReceipt> {
    let full = target.root.join(&target.relative);
    crate::conditional_replace::require_parent(&full, create_parents)?;
    crate::atomic_io::atomic_write_with_durability(&full, contents, durability)
}

#[cfg(all(not(unix), not(windows)))]
fn read_for_replace_scoped_target(target: &ScopedMutationTarget) -> io::Result<Vec<u8>> {
    std::fs::read(target.root.join(&target.relative))
}

#[cfg(all(test, unix))]
pub(super) fn atomic_write_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
) -> io::Result<()> {
    atomic_replace_scoped_target(
        target,
        contents,
        crate::atomic_io::AtomicWriteDurability::Flush,
        true,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_parent_creation_does_not_create_the_parent() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("missing/state.json");
        let target = ScopedMutationTarget {
            root: workspace.path().to_path_buf(),
            relative: PathBuf::from("missing/state.json"),
        };

        let error = atomic_replace_scoped_target(
            &target,
            b"state",
            crate::atomic_io::AtomicWriteDurability::Namespace,
            false,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!path.exists());
        assert!(!workspace.path().join("missing").exists());
    }
}
