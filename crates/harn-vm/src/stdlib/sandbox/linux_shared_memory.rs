//! Where a confined child keeps POSIX semaphores.
//!
//! glibc's `sem_open` creates a temporary file in `/dev/shm`, links it to
//! `/dev/shm/sem.<name>`, unlinks the temporary name, and maps it. Python's
//! `multiprocessing.Lock()` and every process pool built on it do this, so a
//! child with no grant there fails with `PermissionError: [Errno 13]`.
//!
//! A launch with a mount namespace gives the child an empty private tmpfs on
//! `/dev/shm`: bubblewrap mounts it with the other grants, and the namespace
//! helper mounts it beside its private `/tmp` and grants it in the ruleset
//! there. A direct Landlock launch has no namespace, so it grants the host's
//! `/dev/shm` the regular-file rights `sem_open` needs and reports that once at
//! diagnostic level. Other users' segments there stay protected by their file
//! permissions; this user's are reachable by name.

use std::io;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use super::landlock_support::{self, AccessFs, FsAccess};
use super::{push_rule, FilesystemProfile};
use crate::value::VmError;

/// Where glibc keeps POSIX semaphores and shared memory.
pub(super) const SHARED_MEMORY_DIR: &str = "/dev/shm";

/// What `sem_open`, `sem_unlink` and Python's `SemLock` need, measured with
/// strace: create a regular file, open it read/write, `link` it within the
/// same directory (no `REFER`), and unlink names. No listing, directories,
/// symlinks, special files, truncation, or execution.
pub(super) const SHARED_MEMORY_ACCESS: FsAccess = landlock_support::make_bitflags!(AccessFs::{
    MakeReg | ReadFile | WriteFile | RemoveFile
});

/// Grant the host `/dev/shm` to a direct Landlock child. `namespaced` is true
/// when the helper will mount a private one instead: a host rule could not
/// reach that mount, because Landlock skips a hidden mount point.
pub(super) fn grant_host_fallback(
    filesystem: &mut FilesystemProfile,
    namespaced: bool,
) -> Result<(), VmError> {
    if namespaced || !Path::new(SHARED_MEMORY_DIR).is_dir() {
        return Ok(());
    }
    push_rule(
        filesystem,
        PathBuf::from(SHARED_MEMORY_DIR),
        SHARED_MEMORY_ACCESS,
        true,
    )?;
    static REPORTED: std::sync::Once = std::sync::Once::new();
    REPORTED.call_once(|| {
        crate::events::log_debug(
            "handler_sandbox",
            "no mount namespace for this child: the host /dev/shm is granted for regular-file \
             creation, reads, writes and removal, so POSIX semaphores work and the child can \
             reach this user's shared-memory segments there",
        );
    });
    Ok(())
}

/// Mount an empty private tmpfs over `/dev/shm` and grant it to `ruleset`.
/// Runs in the namespace helper, inside its propagation-private mount
/// namespace, before the payload enters the ruleset.
pub(super) fn mount_private(ruleset: Option<RawFd>) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if !Path::new(SHARED_MEMORY_DIR).is_dir() {
        return Ok(());
    }
    let mounted = unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            c"/dev/shm".as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            c"mode=0700".as_ptr().cast(),
        )
    };
    if mounted != 0 {
        return Err(io::Error::last_os_error());
    }
    let Some(ruleset) = ruleset else {
        return Ok(());
    };
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(SHARED_MEMORY_DIR)?;
    landlock_support::add_path_rule(ruleset, &dir, SHARED_MEMORY_ACCESS)
}

#[cfg(test)]
#[path = "linux_shared_memory_tests.rs"]
pub(super) mod tests;
