//! Workspace-root path checks and the scoped mutation entry points.
//!
//! The parent module keeps the shared sandbox API and the platform-specific
//! fd-walk implementations. This module owns the boundary that validates a
//! requested path, selects its writable root, and dispatches a mutation through
//! those implementations.

use std::io;
use std::path::{Path, PathBuf};

use crate::value::VmError;

use super::paths::{
    access_is_exempt_from_scope, is_standard_io_device_for_access, normalize_for_policy,
    normalize_io_device_path, path_is_within,
};
use super::{
    normalized_read_only_roots, normalized_workspace_roots, path_is_denied,
    process_sandbox_read_deny_roots, sandbox_rejection, AppendLockOptions, FsAccess,
    SandboxViolation,
};

/// Check whether `path` is inside the active policy's workspace roots.
///
/// Returns `Ok(())` when no execution policy is active, when the active
/// profile does not enforce path scope, when the normalized path falls within
/// a writable workspace root, or — for [`FsAccess::Read`] only — when it falls
/// within a read-only root. A write/delete that resolves under a read-only root
/// is rejected with `read_only` set, as is any access that falls outside every
/// configured root.
///
/// This is the public, `VmError`-free entry point embedders use to apply
/// workspace-root scoping to their own host calls. The in-crate
/// `harness.fs.*` builtins funnel through [`enforce_fs_path`], which wraps this
/// with a `VmError`; both share the same path normalization and rejection text.
pub fn check_fs_path_scope(path: &Path, access: FsAccess) -> Result<(), SandboxViolation> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if !policy.sandbox_profile.enforces_path_scope() {
        return Ok(());
    }
    // Standard process I/O device files are not workspace filesystem
    // mutations: writing to /dev/stdout, /dev/stderr, or /dev/null (and the
    // numeric /dev/fd/<N> descriptors they alias) targets the process's own
    // output streams, not the sandboxed tree. A pipeline that falls back to
    // /dev/stdout for debug output must not read as a sandbox violation, so
    // allow these regardless of the configured roots. Matched on the
    // lexically-normalized path (not the canonicalized form): canonicalize()
    // rewrites /dev/stdout to a per-process /dev/fd/<…>.output alias that no
    // longer looks like a standard device. Kept deliberately narrow — only
    // the well-known device files, no broader /dev access.
    if access_is_exempt_from_scope(path, access) {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    // The denylist is checked BEFORE any grant, because it must beat all of
    // them. A workspace root, a read-only root, and a preset are each a reason
    // to allow; this is the one reason to refuse, and a subtraction that ran
    // after the grants would never fire on the paths that matter (a credential
    // under a preset-granted `~/.config` is exactly that case).
    if access == FsAccess::Read
        && path_is_denied(&candidate, &process_sandbox_read_deny_roots(&policy))
    {
        return Err(SandboxViolation {
            attempted: candidate,
            roots,
            access,
            read_only: false,
        });
    }
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    let read_only_roots = normalized_read_only_roots(&policy);
    let within_read_only = read_only_roots
        .iter()
        .any(|root| path_is_within(&candidate, root));
    if within_read_only && access == FsAccess::Read {
        return Ok(());
    }
    Err(SandboxViolation {
        attempted: candidate,
        roots,
        access,
        read_only: within_read_only,
    })
}

pub(crate) fn enforce_fs_path(builtin: &str, path: &Path, access: FsAccess) -> Result<(), VmError> {
    check_fs_path_scope(path, access)
        .map_err(|violation| sandbox_rejection(violation.message(builtin)))
}

pub(crate) fn append_scoped_at_open(builtin: &str, path: &Path, contents: &[u8]) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return super::append_unscoped(path, contents);
    };
    super::append_scoped_target(&target, contents)
}

pub(crate) fn append_locked_scoped_at_open(
    builtin: &str,
    path: &Path,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return super::locked_append::append_locked_unscoped(path, contents, options);
    };
    super::locked_append::append_locked_scoped_target(&target, contents, options)
}

pub(crate) fn copy_scoped_at_open(builtin: &str, src: &Path, dst: &Path) -> io::Result<u64> {
    let Some(target) = scoped_mutation_target(builtin, dst, FsAccess::Write)? else {
        return std::fs::copy(src, dst);
    };
    super::copy_scoped_target(src, &target)
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
    super::rename_scoped_targets(&src_target, &dst_target)
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
        super::create_dir_all_scoped_target(&target)
    } else {
        super::create_dir_scoped_target(&target)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ScopedMutationTarget {
    pub(crate) root: PathBuf,
    pub(crate) relative: PathBuf,
}

pub(crate) fn scoped_mutation_target(
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
    let roots = normalized_workspace_roots(&policy);
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
