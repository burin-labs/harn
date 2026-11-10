//! Scoped filesystem mutation: append, copy, rename, and mkdir that resolve a
//! target inside the active workspace root without following a symlink or
//! junction anywhere on the walk.
//!
//! On unix every component is opened `O_NOFOLLOW` relative to its parent
//! descriptor, so the path is never resolved again between the scope check and
//! the write. Windows has no `openat`, so it walks by path and refuses any
//! reparse point it meets. With no active sandbox scope each entrypoint falls
//! back to the plain `std::fs` operation.

use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use super::paths::{
    is_standard_io_device_for_access, normalize_for_policy, normalize_io_device_path,
    path_is_within,
};
use super::{
    check_fs_path_scope, locked_append, normalized_workspace_roots, workspace_env,
    AppendLockOptions, FsAccess,
};

#[cfg(any(unix, windows))]
pub(super) const MAX_SCOPED_PATH_COMPONENTS: usize = 256;

pub(crate) fn append_scoped_at_open(builtin: &str, path: &Path, contents: &[u8]) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return append_unscoped(path, contents);
    };
    append_scoped_target(&target, contents)
}

pub(crate) fn append_locked_scoped_at_open(
    builtin: &str,
    path: &Path,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return locked_append::append_locked_unscoped(path, contents, options);
    };
    locked_append::append_locked_scoped_target(&target, contents, options)
}

pub(crate) fn copy_scoped_at_open(builtin: &str, src: &Path, dst: &Path) -> io::Result<u64> {
    let Some(target) = scoped_mutation_target(builtin, dst, FsAccess::Write)? else {
        return std::fs::copy(src, dst);
    };
    copy_scoped_target(src, &target)
}

pub(crate) fn rename_scoped_at_open(builtin: &str, src: &Path, dst: &Path) -> io::Result<()> {
    let Some(src_target) = scoped_mutation_target(builtin, src, FsAccess::Delete)? else {
        return std::fs::rename(src, dst);
    };
    let dst_target = scoped_mutation_target(builtin, dst, FsAccess::Write)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to rename '{}' without an active destination sandbox scope",
                dst.display()
            ),
        )
    })?;
    rename_scoped_targets(&src_target, &dst_target)
}

pub(crate) fn create_dir_scoped_at_open(
    builtin: &str,
    path: &Path,
    recursive: bool,
) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return if recursive {
            std::fs::create_dir_all(path)
        } else {
            std::fs::create_dir(path)
        };
    };
    if recursive {
        create_dir_all_scoped_target(&target)
    } else {
        create_dir_scoped_target(&target)
    }
}

#[derive(Clone, Debug)]
pub(super) struct ScopedMutationTarget {
    pub(super) root: PathBuf,
    pub(super) relative: PathBuf,
}

pub(super) fn scoped_mutation_target(
    builtin: &str,
    path: &Path,
    access: FsAccess,
) -> io::Result<Option<ScopedMutationTarget>> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(None);
    };
    if !policy.sandbox_profile.enforces_path_scope() {
        return Ok(None);
    }
    if is_standard_io_device_for_access(&normalize_io_device_path(path), access) {
        return Ok(None);
    }
    check_fs_path_scope(path, access).map_err(|violation| {
        io::Error::new(io::ErrorKind::PermissionDenied, violation.message(builtin))
    })?;
    let candidate = normalize_for_policy(path);
    let mut roots = normalized_workspace_roots(&policy);
    roots.extend(workspace_env::owned_runtime_dir_containing(&candidate));
    let Some(root) = roots
        .into_iter()
        .find(|root| path_is_within(&candidate, root))
    else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside writable workspace_roots",
                access.verb(),
                candidate.display()
            ),
        ));
    };
    let relative = candidate.strip_prefix(&root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside workspace root '{}'",
                access.verb(),
                candidate.display(),
                root.display()
            ),
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to {} workspace root '{}'",
                access.verb(),
                root.display()
            ),
        ));
    }
    Ok(Some(ScopedMutationTarget {
        root,
        relative: relative.to_path_buf(),
    }))
}

pub(super) fn append_unscoped(path: &Path, contents: &[u8]) -> io::Result<()> {
    // Match the `append_file` contract: appending to a new log in a
    // not-yet-created directory recreates the parent chain.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(contents))
}

#[cfg(test)]
pub(super) fn shared_atomic_write_unscoped(path: &Path, contents: &[u8]) -> io::Result<()> {
    crate::atomic_io::atomic_write(path, contents)
}

#[cfg(unix)]
pub(super) fn append_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // Append creates the file (and its parent chain) when absent, matching the
    // pre-hardening `append_file` contract (append-to-a-new-log-in-a-new-dir).
    let (parent, file_name) = ensure_parent_dirs_scoped(target)?;
    let mut file = openat_file(
        parent.as_raw_fd(),
        &file_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    file.write_all(contents)
}

#[cfg(windows)]
pub(super) fn append_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
) -> io::Result<()> {
    let (parent, file_name) = win_scoped_parent(target, true)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    append_unscoped(&full, contents)
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn append_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
) -> io::Result<()> {
    let full = target.root.join(&target.relative);
    if let Some(parent) = full.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    append_unscoped(&full, contents)
}

#[cfg(unix)]
pub(super) fn copy_scoped_target(src: &Path, target: &ScopedMutationTarget) -> io::Result<u64> {
    use std::os::fd::AsRawFd;

    let mut source = std::fs::File::open(src)?;
    let source_metadata = source.metadata().ok();
    let (parent, file_name) = open_parent_dir_scoped(target)?;
    let mut destination = openat_file(
        parent.as_raw_fd(),
        &file_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    let copied = io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    if let Some(metadata) = source_metadata {
        let _ = destination.set_permissions(metadata.permissions());
    }
    sync_dir_fd(parent.as_raw_fd());
    Ok(copied)
}

#[cfg(windows)]
pub(super) fn copy_scoped_target(src: &Path, target: &ScopedMutationTarget) -> io::Result<u64> {
    // Copy destinations keep the "parent must already exist" contract, so the
    // walk does not auto-create (create_parents = false), matching the unix
    // `open_parent_dir_scoped` path.
    let (parent, file_name) = win_scoped_parent(target, false)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    std::fs::copy(src, full)
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn copy_scoped_target(src: &Path, target: &ScopedMutationTarget) -> io::Result<u64> {
    std::fs::copy(src, target.root.join(&target.relative))
}

#[cfg(unix)]
pub(super) fn rename_scoped_targets(
    src: &ScopedMutationTarget,
    dst: &ScopedMutationTarget,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let (src_parent, src_name) = open_parent_dir_scoped(src)?;
    let (dst_parent, dst_name) = open_parent_dir_scoped(dst)?;
    renameat_name(
        src_parent.as_raw_fd(),
        &src_name,
        dst_parent.as_raw_fd(),
        &dst_name,
    )?;
    sync_dir_fd(dst_parent.as_raw_fd());
    Ok(())
}

#[cfg(windows)]
pub(super) fn rename_scoped_targets(
    src: &ScopedMutationTarget,
    dst: &ScopedMutationTarget,
) -> io::Result<()> {
    // No `win_reject_reparse_leaf` on the leaves here: rename operates on the
    // directory entry (the name), not by traversing through the target, and may
    // legitimately move/replace a reparse point. The junction-traversal defense
    // is the ancestor-chain validation in `win_scoped_parent`.
    let (src_parent, src_name) = win_scoped_parent(src, false)?;
    let (dst_parent, dst_name) = win_scoped_parent(dst, false)?;
    std::fs::rename(src_parent.join(&src_name), dst_parent.join(&dst_name))
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn rename_scoped_targets(
    src: &ScopedMutationTarget,
    dst: &ScopedMutationTarget,
) -> io::Result<()> {
    std::fs::rename(src.root.join(&src.relative), dst.root.join(&dst.relative))
}

#[cfg(unix)]
pub(super) fn create_dir_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, file_name) = open_parent_dir_scoped(target)?;
    mkdirat_name(parent.as_raw_fd(), &file_name)?;
    sync_dir_fd(parent.as_raw_fd());
    Ok(())
}

#[cfg(windows)]
pub(super) fn create_dir_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    // Single `mkdir` keeps the "parent must already exist" contract; only the
    // leaf is created, after verifying no ancestor is a junction/symlink. No
    // `win_reject_reparse_leaf` on the leaf: `CreateDirectoryW` creates a NEW
    // name and fails `AlreadyExists` if anything (reparse point or not) already
    // occupies it — it never writes *through* an existing leaf — so the
    // ancestor-chain validation is the whole defense.
    let (parent, file_name) = win_scoped_parent(target, false)?;
    win_create_dir_raw(&parent.join(&file_name))
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn create_dir_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    std::fs::create_dir(target.root.join(&target.relative))
}

#[cfg(unix)]
pub(super) fn create_dir_all_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let root = open_dir_absolute(&target.root)?;
    let mut current = root;
    for component in clean_relative_components(&target.relative)? {
        match open_dir_at(current.as_raw_fd(), &component) {
            Ok(next) => current = next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                mkdirat_name(current.as_raw_fd(), &component)?;
                let next = open_dir_at(current.as_raw_fd(), &component)?;
                current = next;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn create_dir_all_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    // `mkdir -p`: every component (including the leaf) is created, and each is
    // verified not to be a reparse point (junction/symlink) as the walk descends.
    let components = win_clean_relative_components(&target.relative)?;
    win_walk_components(&target.root, &components, true)?;
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn create_dir_all_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    std::fs::create_dir_all(target.root.join(&target.relative))
}

#[cfg(unix)]
/// Create the ancestor directory chain of a scoped write/append target,
/// mirroring the pre-hardening `mkdir -p` behavior of the content-producing
/// filesystem builtins (`write_file`, `write_file_bytes`, `append_file`,
/// `append_file_locked`) and `http_download`. Only the ancestors are created —
/// the final path component
/// is the file the caller writes. Traversal stays scoped to `target.root` and
/// symlink-safe (each level is opened with `O_NOFOLLOW` via `open_dir_at`), so
/// this preserves the security properties #4147 added while restoring the
/// directory-autovivification contract downstream code depends on. Concurrent
/// creators are tolerated (a losing `mkdirat` that sees `EEXIST` is ignored).
///
/// The returned parent fd is the one content-producing callers must use for
/// their final `openat`/`renameat`, so the path is not resolved again between
/// mkdir-p and the write.
///
/// Structural operations (copy destination, rename, remove, single `mkdir`)
/// intentionally do NOT call this — they keep `open_parent_dir_scoped`'s
/// "parent must already exist" semantics.
#[cfg(unix)]
pub(super) fn ensure_parent_dirs_scoped(
    target: &ScopedMutationTarget,
) -> io::Result<(std::os::fd::OwnedFd, String)> {
    use std::os::fd::AsRawFd;

    let mut components = clean_relative_components(&target.relative)?;
    let file_name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox scoped open requires a file name: {}",
                target.relative.display()
            ),
        )
    })?;
    let root = open_dir_absolute(&target.root)?;
    let mut current = root;
    for component in components {
        match open_dir_at(current.as_raw_fd(), &component) {
            Ok(next) => current = next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Err(mkerr) = mkdirat_name(current.as_raw_fd(), &component) {
                    if mkerr.kind() != io::ErrorKind::AlreadyExists {
                        return Err(mkerr);
                    }
                }
                current = open_dir_at(current.as_raw_fd(), &component)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok((current, file_name))
}

#[cfg(unix)]
pub(super) fn open_parent_dir_scoped(
    target: &ScopedMutationTarget,
) -> io::Result<(std::os::fd::OwnedFd, String)> {
    use std::os::fd::AsRawFd;

    let mut components = clean_relative_components(&target.relative)?;
    let file_name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox scoped open requires a file name: {}",
                target.relative.display()
            ),
        )
    })?;
    let root = open_dir_absolute(&target.root)?;
    let mut current = root;
    for component in components {
        current = open_dir_at(current.as_raw_fd(), &component)?;
    }
    Ok((current, file_name))
}

#[cfg(unix)]
pub(super) fn clean_relative_components(path: &Path) -> io::Result<Vec<String>> {
    use std::os::unix::ffi::OsStrExt;

    let mut out = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let bytes = value.as_bytes();
                if bytes.contains(&0) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("path component contains NUL: {}", path.display()),
                    ));
                }
                out.push(value.to_string_lossy().into_owned());
                if out.len() > MAX_SCOPED_PATH_COMPONENTS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "sandbox scoped path exceeds {MAX_SCOPED_PATH_COMPONENTS} components: {}",
                            path.display()
                        ),
                    ));
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("sandbox scoped path must stay relative: {}", path.display()),
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(unix)]
pub(super) fn open_dir_absolute(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path contains NUL: {}", path.display()),
        )
    })?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
pub(super) fn open_dir_at(parent_fd: libc::c_int, name: &str) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};

    let c_name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent_fd,
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
pub(super) fn openat_file(
    parent_fd: libc::c_int,
    name: &str,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;

    let c_name = c_name(name)?;
    let fd = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
pub(super) fn mkdirat_name(parent_fd: libc::c_int, name: &str) -> io::Result<()> {
    let c_name = c_name(name)?;
    let rc = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o777) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn renameat_name(
    old_parent_fd: libc::c_int,
    old_name: &str,
    new_parent_fd: libc::c_int,
    new_name: &str,
) -> io::Result<()> {
    let old_name = c_name(old_name)?;
    let new_name = c_name(new_name)?;
    let rc = unsafe {
        libc::renameat(
            old_parent_fd,
            old_name.as_ptr(),
            new_parent_fd,
            new_name.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn unlinkat_name(
    parent_fd: libc::c_int,
    name: &str,
    flags: libc::c_int,
) -> io::Result<()> {
    let c_name = c_name(name)?;
    let rc = unsafe { libc::unlinkat(parent_fd, c_name.as_ptr(), flags) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_dir_fd(fd: libc::c_int) -> bool {
    (unsafe { libc::fsync(fd) }) == 0
}

#[cfg(unix)]
pub(super) fn c_name(name: &str) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path component contains NUL: {name:?}"),
        )
    })
}

// ---------------------------------------------------------------------------
// Windows scoped-walk primitives (junction/symlink-safe directory descent).
//
// Windows has no `openat`, and `O_NOFOLLOW` has no equivalent that a plain
// `std::fs` path open honors — worse, a *junction* (mount-point reparse point)
// IS a directory and is creatable by a non-admin user, so it slips past every
// "is this a symlink" check that only inspects the leaf. The unix path defends
// the whole chain by opening each component `O_NOFOLLOW`; the Windows path here
// mirrors that by opening each walked component with
// `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` (so the reparse
// point itself is opened, not its target) and refusing the walk the moment any
// component reports a mount-point or symlink reparse tag. See
// research/scoped-fs-mkdir-footguns (#12, RedirectionGuard) for the class.
//
// Residual, Windows-CI-only: because there is no handle-relative openat here,
// each component is re-resolved by string as the walk descends, so a
// concurrent attacker who swaps an *already-validated* ancestor for a junction
// between our check and the next open is not fully closed (the unix fd-walk is;
// the true fix is `NtCreateFile` with a `RootDirectory` handle). The
// intermediate-junction class the acceptance test covers IS closed.
// ---------------------------------------------------------------------------

/// Reparse tags Windows assigns to the two "traverses out of the tree"
/// reparse-point kinds the scoped walk must refuse. Defined locally so the
/// module does not need the `Win32_System_SystemServices` feature just for two
/// stable ABI constants.
#[cfg(windows)]
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
#[cfg(windows)]
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

#[cfg(windows)]
pub(super) fn win_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Refuse `path` if it is a mount-point or symlink reparse point. The handle is
/// opened with `FILE_FLAG_OPEN_REPARSE_POINT` so we inspect the reparse point
/// itself rather than following it, and `FILE_FLAG_BACKUP_SEMANTICS` so a
/// directory handle is permitted. A `NotFound` error is propagated unchanged so
/// callers can distinguish "does not exist yet" (create it) from "exists and is
/// hostile" (refuse).
#[cfg(windows)]
pub(super) fn win_reject_reparse_point(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, GetFileInformationByHandleEx,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    // Query attributes only; no read/write access to the object is needed.
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;

    let wide = win_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    let result = if ok == 0 {
        Err(io::Error::last_os_error())
    } else if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && matches!(
            info.ReparseTag,
            IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK
        )
    {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox scoped walk refuses reparse-point (junction/symlink) component: {}",
                path.display()
            ),
        ))
    } else {
        Ok(())
    };
    unsafe {
        CloseHandle(handle);
    }
    result
}

/// A reparse point that squats on a *leaf* target name is refused; a leaf that
/// simply does not exist yet is fine (the caller is about to create it).
#[cfg(windows)]
pub(super) fn win_reject_reparse_leaf(path: &Path) -> io::Result<()> {
    match win_reject_reparse_point(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Low-level `CreateDirectoryW` used by the scoped walk. Kept raw (rather than
/// `std::fs::create_dir`) so the recurrence-guard lint can assert the scoped
/// Windows walk never reaches for a path-based `std::fs` mutation.
#[cfg(windows)]
pub(super) fn win_create_dir_raw(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let wide = win_wide(path);
    let ok = unsafe { CreateDirectoryW(wide.as_ptr(), std::ptr::null()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Windows analogue of [`clean_relative_components`]: reject `..`, absolute, and
/// drive-prefixed components, cap the depth, and refuse embedded NULs — keeping
/// the same invariants the unix walk enforces before descending.
#[cfg(windows)]
pub(super) fn win_clean_relative_components(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    use std::os::windows::ffi::OsStrExt;

    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.encode_wide().any(|unit| unit == 0) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("path component contains NUL: {}", path.display()),
                    ));
                }
                out.push(value.to_os_string());
                if out.len() > MAX_SCOPED_PATH_COMPONENTS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "sandbox scoped path exceeds {MAX_SCOPED_PATH_COMPONENTS} components: {}",
                            path.display()
                        ),
                    ));
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("sandbox scoped path must stay relative: {}", path.display()),
                ));
            }
        }
    }
    Ok(out)
}

/// Descend `root` through `components`, refusing any component that is a
/// junction/symlink reparse point. When `create` is set, missing directories
/// are created (`mkdir -p`) and re-validated immediately, so a directory we
/// just made cannot be a reparse point. Returns the validated deepest path.
#[cfg(windows)]
pub(super) fn win_walk_components(
    root: &Path,
    components: &[std::ffi::OsString],
    create: bool,
) -> io::Result<PathBuf> {
    // The configured workspace root is trusted, but verify it resolves to a real
    // directory and is not itself a reparse point, mirroring the unix
    // `open_dir_absolute` `O_NOFOLLOW` open of the root.
    win_reject_reparse_point(root)?;
    let mut current = root.to_path_buf();
    for component in components {
        current.push(component);
        match win_reject_reparse_point(&current) {
            Ok(()) => {}
            Err(err) if create && err.kind() == io::ErrorKind::NotFound => {
                match win_create_dir_raw(&current) {
                    Ok(()) => {}
                    // A concurrent creator won the race; tolerate and re-validate.
                    Err(mkerr) if mkerr.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(mkerr) => return Err(mkerr),
                }
                win_reject_reparse_point(&current)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(current)
}

/// Validate the ancestor chain of a scoped target on Windows and return the
/// verified `(parent_dir, leaf_name)`. With `create_parents`, missing ancestors
/// are created; without it, the parent must already exist (structural ops).
#[cfg(windows)]
pub(super) fn win_scoped_parent(
    target: &ScopedMutationTarget,
    create_parents: bool,
) -> io::Result<(PathBuf, std::ffi::OsString)> {
    let mut components = win_clean_relative_components(&target.relative)?;
    let file_name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox scoped open requires a file name: {}",
                target.relative.display()
            ),
        )
    })?;
    let parent = win_walk_components(&target.root, &components, create_parents)?;
    Ok((parent, file_name))
}
