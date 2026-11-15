//! Projections of a capability policy onto the operating-system sandbox
//! backends: the directory sets a confined child is allowed to touch, and the
//! disposition the active backend will report for a Unix-socket grant it was
//! asked to render.
//!
//! These sit apart from the spawn machinery because they are readings of the
//! policy. Nothing here mutates state, and the only host fact consulted is
//! whether Linux Landlock can render the serve-only socket grant, so a backend
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
    trusted_process_roots(policy, &policy.process_sandbox.read_roots, RootUse::Read)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn process_sandbox_policy_write_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    trusted_process_roots(policy, &policy.process_sandbox.write_roots, RootUse::Write)
}

/// Every directory a child confined by `policy` may write under, made
/// canonical: the workspace roots (with their git topology and runtime dir),
/// the session temp dir, the policy's process write roots, the
/// developer-toolchain caches, the Unix-socket roots and, on Linux, the host
/// `/dev/shm` a direct Landlock launch falls back to granting.
///
/// Every root goes through [`super::normalize_for_policy`], the same
/// normalization the backends render from: a root that does not exist yet
/// resolves through its deepest existing ancestor, and macOS's `/tmp` and
/// `/var` aliases read as `/private/...`, so a reader comparing canonical
/// paths cannot miss a file a child creates there later.
///
/// A host that reads a file before any sandbox exists asks this whether a
/// confined child could have planted it. The set names every root a backend
/// may grant write under, including those a policy that withholds child
/// writes renders read-only, so a reader that refuses these trusts too
/// little rather than too much.
pub fn process_sandbox_writable_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = super::process_sandbox_roots(policy);
    roots.extend(super::workspace_local_tmpdir(policy));
    roots.extend(normalized_process_roots(
        &policy.process_sandbox.write_roots,
    ));
    roots.extend(normalized_process_roots(
        &policy.process_sandbox.unix_socket_roots,
    ));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    roots.extend(super::process_sandbox_developer_toolchain_cache_roots(
        policy,
    ));
    #[cfg(target_os = "linux")]
    roots.extend(super::linux::shared_memory_fallback_root());
    let mut roots: Vec<PathBuf> = roots
        .iter()
        .map(|root| super::normalize_for_policy(root))
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Directories a confined child may place a Unix-domain socket under.
#[cfg(target_os = "linux")]
pub(crate) fn process_sandbox_unix_socket_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    trusted_process_roots(
        policy,
        &policy.process_sandbox.unix_socket_roots,
        RootUse::Write,
    )
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

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RootUse {
    Read,
    Write,
}

/// The policy's declared process roots, resolved once per spawn and kept only
/// where the resolution can be trusted.
///
/// A root is resolved again for every spawn, so a link along its path decides
/// where the grant lands. A link that sits in a directory a confined child can
/// write, or that a workspace checkout brought with it, is not the host's
/// choice: one spawn could replace `<workspace>/target` with a link to the
/// user's configuration and the next spawn would grant there. Such a root is
/// refused. So is a write or socket root that resolves into, onto, or over a
/// file git, gh or ssh read as the user's own configuration.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
fn trusted_process_roots(
    policy: &CapabilityPolicy,
    declared: &[String],
    usage: RootUse,
) -> Vec<PathBuf> {
    if declared.is_empty() {
        return Vec::new();
    }
    let child_writable = process_sandbox_writable_roots(policy);
    let protected = match usage {
        RootUse::Write => super::sandbox_user_home_dir()
            .map(|home| super::workspace_env::grant_spawn::trusted_config_paths(&home))
            .unwrap_or_default(),
        RootUse::Read => Vec::new(),
    };
    declared
        .iter()
        .zip(normalized_process_roots(declared))
        .filter_map(|(root, resolved)| {
            let lexical = super::paths::lexical_for_policy(std::path::Path::new(root));
            if let Some(link) = planted_link(&lexical, &child_writable) {
                refuse_root(root, &format!("it is reached through {}", link.display()));
                return None;
            }
            if let Some(config) = protected
                .iter()
                .find(|config| resolved.starts_with(config) || config.starts_with(&resolved))
            {
                refuse_root(
                    root,
                    &format!("it would let a child write {}", config.display()),
                );
                return None;
            }
            Some(resolved)
        })
        .collect()
}

/// The first link on `lexical`'s path whose own directory a confined child can
/// write, if any.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
fn planted_link(lexical: &std::path::Path, child_writable: &[PathBuf]) -> Option<PathBuf> {
    lexical
        .ancestors()
        .filter(|ancestor| {
            ancestor
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
        .find(|link| {
            link.parent().is_some_and(|parent| {
                let parent = super::normalize_for_policy(parent);
                child_writable.iter().any(|root| parent.starts_with(root))
            })
        })
        .map(std::path::Path::to_path_buf)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
fn refuse_root(root: &str, reason: &str) {
    let message = format!("process sandbox root {root} is not granted: {reason}");
    super::warn_once(&message, &message);
}

#[cfg(all(test, unix))]
#[path = "policy_projection_tests.rs"]
mod tests;
