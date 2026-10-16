//! Confining this process itself with the Landlock ruleset a child would get.
//!
//! Filesystem only. The seccomp allowlist the backend compiles is a child's
//! syscall ceiling, sized for the programs a confined command runs, not for a
//! server's runtime; installing it here would trade a filesystem boundary for
//! a process that dies on its first unlisted syscall.

use std::os::fd::AsRawFd;

use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::sandbox_rejection;
use crate::value::VmError;

pub(super) fn confine(policy: &CapabilityPolicy) -> Result<(), VmError> {
    // Landlock confines the calling thread and the threads it later creates,
    // never threads that already exist. One confined thread among unconfined
    // ones shares their memory and so their authority, which is no boundary.
    let threads = std::fs::read_dir("/proc/self/task")
        .map(|tasks| tasks.count())
        .map_err(|error| {
            sandbox_rejection(format!("cannot count this process's threads: {error}"))
        })?;
    if threads != 1 {
        return Err(sandbox_rejection(format!(
            "Landlock confines only the calling thread, and this process already runs {threads} \
             threads; confine it before starting any other"
        )));
    }
    // The same negotiated Landlock a child gets, with one more requirement: a
    // child below ABI 3 has its truncation gap closed by the seccomp filter,
    // and this process installs no filter, so it needs the kernel to close it.
    let support = super::host_landlock(policy.sandbox_profile)?;
    if !support.bounds_truncation() {
        return Err(sandbox_rejection(format!(
            "confining this process needs Landlock ABI {} to bound truncation, because no syscall \
             filter backs it here; this host reports ABI {}",
            super::ABI::V3,
            support.abi()
        )));
    }
    let program = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let landlock = super::landlock_profile_for(&program, policy, support)?;
    super::landlock_support::enter_ruleset(landlock.ruleset.as_raw_fd())
        .and_then(|()| super::confirm_filesystem_boundary_holds())
        .map_err(|error| sandbox_rejection(format!("failed to confine this process: {error}")))
}
