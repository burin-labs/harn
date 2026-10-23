//! Projections of a capability policy onto the operating-system sandbox
//! backends: the directory sets a confined child is allowed to touch, and the
//! disposition the active backend will report for a Unix-socket grant it was
//! asked to render.
//!
//! These sit apart from the spawn machinery so backends and receipt writers
//! use the same effective roots. Run-private storage is provisioned by the
//! workspace directory owner; Linux also measures Landlock socket support.

#[allow(unused_imports)]
use std::path::PathBuf;

#[allow(unused_imports)]
use crate::orchestration::CapabilityPolicy;
use crate::orchestration::UnixSocketEnforcement;

#[allow(unused_imports)]
use super::normalized_process_roots;
#[cfg(target_os = "linux")]
use super::policy_allows_network;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn process_sandbox_policy_read_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_process_roots(&policy.process_sandbox.read_roots)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn process_sandbox_policy_write_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_process_roots(&policy.process_sandbox.write_roots)
}

/// Directories a confined child may place a Unix-domain socket under.
pub(crate) fn process_sandbox_unix_socket_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = policy.process_sandbox.unix_socket_roots.clone();
    if policy.process_sandbox.allow_run_temp_sockets {
        if let Some(path) = super::workspace_local_tmpdir(policy) {
            roots.push(path.display().to_string());
        }
    }
    normalized_process_roots(&roots)
}

pub(super) fn run_temp_socket_refusal(policy: &CapabilityPolicy) -> Option<&'static str> {
    if !policy.process_sandbox.allow_run_temp_sockets {
        return None;
    }
    #[cfg(target_os = "linux")]
    return Some("Linux cannot enforce run-private Unix socket clients: the socket client operation cannot be scoped to one path");
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Some("this platform cannot enforce run-private Unix socket access");
    #[cfg(target_os = "macos")]
    None
}

/// What the active backend will actually do with a Unix-socket grant.
///
/// A reader of a receipt must not have to infer the shape of the grant from
/// the platform it ran on, and must not read silence as a scope that was
/// applied. Every backend answers, including when it refuses.
pub fn unix_socket_enforcement(policy: &CapabilityPolicy) -> UnixSocketEnforcement {
    if !policy.process_sandbox.requests_unix_sockets() {
        return UnixSocketEnforcement::NotRequested;
    }
    if run_temp_socket_refusal(policy).is_some()
        || policy.process_sandbox.allow_run_temp_sockets
            && super::workspace_env::run_temp_directory(policy).is_err()
    {
        return UnixSocketEnforcement::Refused;
    }
    #[cfg(target_os = "macos")]
    {
        UnixSocketEnforcement::PathScoped
    }
    #[cfg(target_os = "linux")]
    {
        if policy_allows_network(policy) {
            UnixSocketEnforcement::SupersededByNetworkGrant
        } else if serve_only_unavailable_on_host() {
            UnixSocketEnforcement::Refused
        } else {
            UnixSocketEnforcement::ServeOnly
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        UnixSocketEnforcement::Refused
    }
}

/// The one host fact the Linux disposition reads: Landlock below ABI 6
/// cannot contain abstract sockets, so the backend refuses the serve-only
/// grant there instead of rendering it.
#[cfg(target_os = "linux")]
pub(crate) fn serve_only_unavailable_on_host() -> bool {
    super::active_backend_mechanism_limits()
        .contains(&super::enforcement::MechanismLimit::UnixSocketGrantUnavailable)
}
