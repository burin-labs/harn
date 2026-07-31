//! Permission enforcement shared by the path-touching hostlib builtins.
//!
//! Authority to invoke these operations is carried by the `HarnessTools`
//! handle. This module owns only the path-level scope check shared by the
//! filesystem-touching implementations.

use std::path::Path;

use harn_vm::process_sandbox::{check_fs_path_scope, FsAccess};

use crate::error::HostlibError;

/// Reject `path` when it resolves outside the active execution policy's
/// workspace roots, under a restricted sandbox profile.
///
/// This is the single path-scope policy shared by every hostlib builtin
/// that resolves a host filesystem path — the `tools::*`, `fs::*`, and
/// `ast::*` surfaces — so the granular workspace-root check stays
/// consistent across all of them (the path-level complement to the coarse
/// It delegates to [`harn_vm::process_sandbox::check_fs_path_scope`] so a path the
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
