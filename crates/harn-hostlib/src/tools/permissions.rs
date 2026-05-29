//! Permission enforcement shared by the path-touching hostlib builtins.
//!
//! Two complementary layers live here:
//!
//! * The per-thread "enabled features" registry plus [`gated_handler`] —
//!   the coarse "is this session allowed to touch the filesystem at all?"
//!   gate (see the module body below).
//! * [`enforce_path_scope`] — the granular "is this *specific path* inside
//!   the session's workspace roots?" check, delegating to the VM-native
//!   sandbox so the hostlib surface and `harness.fs.*` agree.
//!
//! `harn-hostlib` exposes the deterministic tool builtins on every VM that
//! `install_default` runs against, but pipelines must explicitly opt in to
//! their use by calling the `hostlib_enable("tools:deterministic")` builtin
//! before any of the tool methods will execute. This keeps the surface
//! sandbox-friendly: a script that doesn't ask for the tools cannot poke
//! the host filesystem or shell out to `git` even though the contract is
//! registered.
//!
//! State is held in a thread-local so that:
//!
//! * Independent VM runs stay isolated when the embedder executes them on
//!   separate threads.
//! * Cargo test isolation works without extra ceremony.
//!
//! Embedders can also call [`enable_for_test`] / [`reset`] from Rust if
//! they need to bypass the builtin (for example, tests that don't drive
//! a live VM).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use harn_vm::process_sandbox::{check_fs_path_scope, FsAccess};
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::SyncHandler;

/// Feature key for the deterministic-tools surface.
///
/// Kept here as a constant so [`tools::register_builtins`](super::register_builtins)
/// and the integration tests share the exact same string.
pub const FEATURE_TOOLS_DETERMINISTIC: &str = "tools:deterministic";

thread_local! {
    static ENABLED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// Mark `feature` as enabled on the current thread. Returns `true` if the
/// feature was newly enabled, `false` if it was already on.
pub fn enable(feature: &str) -> bool {
    ENABLED.with(|cell| cell.borrow_mut().insert(feature.to_string()))
}

/// Mark `feature` as disabled on the current thread. Returns `true` if the
/// feature was previously enabled. Mostly useful in tests that want to
/// assert the gate works.
pub fn disable(feature: &str) -> bool {
    ENABLED.with(|cell| cell.borrow_mut().remove(feature))
}

/// Bulk-clear every enabled feature on the current thread. Tests use this
/// to start from a known state.
pub fn reset() {
    ENABLED.with(|cell| cell.borrow_mut().clear());
}

/// Report whether `feature` is enabled on the current thread.
pub fn is_enabled(feature: &str) -> bool {
    ENABLED.with(|cell| cell.borrow().contains(feature))
}

/// Convenience wrapper for tests: enable the deterministic tools in the
/// current thread without needing to reach for the builtin.
pub fn enable_for_test() {
    enable(FEATURE_TOOLS_DETERMINISTIC);
}

/// Wrap a builtin runner so it executes only when the deterministic-tools
/// feature has been enabled on the current thread, returning a descriptive
/// error otherwise.
///
/// This is the single gating policy shared by every hostlib builtin that
/// reads or writes arbitrary host filesystem paths — the `tools::*` file
/// I/O surface plus the `fs::*` and `ast::*` edit helpers — so a script
/// denied `tools:deterministic` cannot mutate the working tree through any
/// of them.
pub fn gated_handler(
    name: &'static str,
    runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
) -> SyncHandler {
    Arc::new(move |args: &[VmValue]| {
        if !is_enabled(FEATURE_TOOLS_DETERMINISTIC) {
            return Err(HostlibError::Backend {
                builtin: name,
                message: format!(
                    "feature `{FEATURE_TOOLS_DETERMINISTIC}` is not enabled in this \
                     session — call `hostlib_enable(\"{FEATURE_TOOLS_DETERMINISTIC}\")` \
                     before invoking deterministic tools"
                ),
            });
        }
        runner(args)
    })
}

/// Reject `path` when it resolves outside the active execution policy's
/// workspace roots, under a restricted sandbox profile.
///
/// This is the single path-scope policy shared by every hostlib builtin
/// that resolves a host filesystem path — the `tools::*`, `fs::*`, and
/// `ast::*` surfaces — so the granular workspace-root check stays
/// consistent across all of them (the path-level complement to the coarse
/// [`gated_handler`] feature gate). It delegates to
/// [`harn_vm::process_sandbox::check_fs_path_scope`] so a path the
/// `harness.fs.*` VM-native builtins would refuse is refused here too, with
/// the same message. A no-op when no policy is active or the profile is
/// unrestricted.
pub fn enforce_path_scope(
    builtin: &'static str,
    path: &Path,
    access: FsAccess,
) -> Result<(), HostlibError> {
    check_fs_path_scope(path, access).map_err(|violation| HostlibError::SandboxViolation {
        builtin,
        path: violation.attempted.display().to_string(),
        message: violation.message(builtin),
    })
}
