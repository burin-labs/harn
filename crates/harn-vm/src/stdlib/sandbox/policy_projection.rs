//! Projections of a capability policy onto the operating-system sandbox
//! backends: the directory sets a confined child is allowed to touch, and the
//! disposition the active backend will report for a Unix-socket grant it was
//! asked to render.
//!
//! These sit apart from the spawn machinery because they are pure readings of
//! the policy. Nothing here consults the host or mutates state, so a backend
//! and a receipt writer can ask the same question and get the same answer.

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
#[cfg(target_os = "linux")]
pub(crate) fn process_sandbox_unix_socket_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_process_roots(&policy.process_sandbox.unix_socket_roots)
}

/// What the active backend will actually do with a Unix-socket grant.
///
/// A reader of a receipt must not have to infer the shape of the grant from
/// the platform it ran on, and must not read silence as a scope that was
/// applied. Every backend answers, including when it refuses.
pub fn unix_socket_enforcement(policy: &CapabilityPolicy) -> UnixSocketEnforcement {
    if policy.process_sandbox.unix_socket_roots.is_empty() {
        return UnixSocketEnforcement::NotRequested;
    }
    #[cfg(target_os = "macos")]
    {
        UnixSocketEnforcement::PathScoped
    }
    #[cfg(target_os = "linux")]
    {
        if policy_allows_network(policy) {
            UnixSocketEnforcement::SupersededByNetworkGrant
        } else {
            UnixSocketEnforcement::ServeOnly
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        UnixSocketEnforcement::Refused
    }
}
