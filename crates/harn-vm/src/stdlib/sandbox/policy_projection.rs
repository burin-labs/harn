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
/// `/dev/shm` a direct Landlock launch falls back to granting. A granted Git
/// clone's private transfer directory is included while its profile is built.
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
    #[cfg(target_os = "macos")]
    roots.extend(super::workspace_env::grant_spawn::own_spawn_write_roots());
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

/// What a confined child may do under a host-declared root.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootUse {
    Read,
    /// Write, create or bind: the user's own configuration is refused too.
    Write,
}

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
    host_resolved_roots(policy, declared.iter().map(PathBuf::from), usage)
}

/// Whether a link in `dir` is one a confined child under `policy` could have
/// placed: `dir` is under a root the child can write. The writable set is
/// computed only when a declared root first crosses a link.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn link_dir_is_untrusted(
    policy: &CapabilityPolicy,
) -> impl FnMut(&std::path::Path) -> bool + '_ {
    let mut child_writable: Option<Vec<PathBuf>> = None;
    move |dir| {
        child_writable
            .get_or_insert_with(|| process_sandbox_writable_roots(policy))
            .iter()
            .any(|root| dir.starts_with(root))
    }
}

/// Host-declared roots, each resolved only through links the host placed.
///
/// A root is resolved for every spawn, so a link along its path decides where
/// the grant lands. A link that sits in a directory a confined child can
/// write, or that a workspace checkout brought with it, is not the host's
/// choice, and a root reached through one is refused. So is a write root that
/// resolves into, onto, or over a file git, gh or ssh read as the user's own
/// configuration. Each refusal is reported once.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn host_resolved_roots(
    policy: &CapabilityPolicy,
    declared: impl IntoIterator<Item = PathBuf>,
    usage: RootUse,
) -> Vec<PathBuf> {
    let mut untrusted = link_dir_is_untrusted(policy);
    let protected = match usage {
        RootUse::Write => crate::user_dirs::home_dir()
            .filter(|home| home.is_absolute())
            .map(|home| super::workspace_env::grant_spawn::trusted_config_paths(&home))
            .unwrap_or_default(),
        RootUse::Read => Vec::new(),
    };
    let mut roots = Vec::new();
    for root in declared {
        let root = super::resolve_policy_path(&root.to_string_lossy());
        let resolved = match super::paths::resolve_through_host_links(&root, &mut untrusted) {
            Ok(resolved) => resolved,
            Err(link) => {
                refuse_root(
                    &root,
                    &format!(
                        "it is reached through the link {}, which sits where a confined process \
                         can write; declare the directory it should resolve to instead",
                        link.display()
                    ),
                );
                continue;
            }
        };
        if let Some(config) = protected
            .iter()
            .find(|config| resolved.starts_with(config) || config.starts_with(&resolved))
        {
            refuse_root(
                &root,
                &format!(
                    "it would let a confined process write {}; declare a narrower directory",
                    config.display()
                ),
            );
            continue;
        }
        if !roots.contains(&resolved) {
            roots.push(resolved);
        }
    }
    roots
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
fn refuse_root(root: &std::path::Path, reason: &str) {
    let message = format!(
        "process sandbox root {} is not granted: {reason}",
        root.display()
    );
    super::warn_once(&message, &message);
}

#[cfg(all(test, unix))]
#[path = "policy_projection_tests.rs"]
mod tests;
