//! Cross-process lock coordination for immediate safe-text patches.

use std::cell::RefCell;
use std::fs::{self as stdfs, File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::error::HostlibError;

use super::{normalize_logical, SAFE_TEXT_PATCH_BUILTIN};

thread_local! {
    static EXECUTION_LOCK_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Restores the prior execution-local safe-text-patch lock root on drop.
#[derive(Debug)]
#[must_use = "retain this guard for the execution that owns the lock root"]
pub struct ScopedSafeTextPatchLockRoot {
    previous: Option<PathBuf>,
}

/// Route immediate safe-text-patch locks through a caller-owned execution root.
///
/// The override is thread-local so concurrent VM executions can keep their
/// durable state separate without changing the process-wide production lock
/// namespace. Callers should pass the `fs-cas-locks` directory under their
/// execution state root and retain the returned guard for the execution.
pub fn scope_safe_text_patch_lock_root(root: impl AsRef<Path>) -> ScopedSafeTextPatchLockRoot {
    let root = root.as_ref().to_path_buf();
    let previous = EXECUTION_LOCK_ROOT.with(|slot| slot.replace(Some(root)));
    ScopedSafeTextPatchLockRoot { previous }
}

impl Drop for ScopedSafeTextPatchLockRoot {
    fn drop(&mut self) {
        EXECUTION_LOCK_ROOT.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

pub(super) fn safe_text_patch_lock_root() -> PathBuf {
    if let Some(root) = EXECUTION_LOCK_ROOT.with(|slot| slot.borrow().clone()) {
        return root;
    }
    let runtime_root = harn_vm::stdlib::process::runtime_root_base();
    harn_vm::runtime_paths::state_root(&runtime_root).join("fs-cas-locks")
}

pub(super) fn acquire_safe_text_patch_lock(
    path: &Path,
    lock_root: &Path,
) -> Result<SafeTextPatchLock, HostlibError> {
    let file = open_safe_text_patch_lock(path, lock_root)?;
    file.lock_exclusive()
        .map_err(|error| HostlibError::Backend {
            builtin: SAFE_TEXT_PATCH_BUILTIN,
            message: format!("acquire CAS lock for `{}`: {error}", path.display()),
        })?;
    Ok(SafeTextPatchLock { file })
}

pub(super) fn open_safe_text_patch_lock(
    path: &Path,
    lock_root: &Path,
) -> Result<File, HostlibError> {
    stdfs::create_dir_all(lock_root).map_err(|error| HostlibError::Backend {
        builtin: SAFE_TEXT_PATCH_BUILTIN,
        message: format!(
            "create CAS lock directory `{}`: {error}",
            lock_root.display()
        ),
    })?;
    let identity = canonical_lock_identity(path);
    let lock_name = format!(
        "{}.lock",
        hex::encode(Sha256::digest(lock_identity_bytes(&identity)))
    );
    let lock_path = lock_root.join(lock_name);
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| HostlibError::Backend {
            builtin: SAFE_TEXT_PATCH_BUILTIN,
            message: format!("open CAS lock `{}`: {error}", lock_path.display()),
        })
}

fn lock_identity_bytes(identity: &Path) -> Vec<u8> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // These platforms commonly use case-insensitive filesystems. Folding
        // only the private lock key safely over-serializes case-sensitive
        // volumes while preventing two spellings of a missing target from
        // acquiring different locks before the first create.
        identity.to_string_lossy().to_lowercase().into_bytes()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        identity.as_os_str().as_encoded_bytes().to_vec()
    }
}

fn canonical_lock_identity(path: &Path) -> PathBuf {
    if let Ok(canonical) = stdfs::canonicalize(path) {
        return canonical;
    }
    let absolute = normalize_logical(path);
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while let Some(name) = ancestor.file_name() {
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if let Ok(canonical_parent) = stdfs::canonicalize(parent) {
            let mut identity = canonical_parent;
            for component in suffix.iter().rev() {
                identity.push(component);
            }
            return identity;
        }
        ancestor = parent;
    }
    absolute
}

pub(super) struct SafeTextPatchLock {
    file: File,
}

impl Drop for SafeTextPatchLock {
    fn drop(&mut self) {
        // Keep the lock file itself: unlinking it can split waiters across two
        // inodes. The OS releases ownership automatically when a process exits.
        let _ = FileExt::unlock(&self.file);
    }
}
