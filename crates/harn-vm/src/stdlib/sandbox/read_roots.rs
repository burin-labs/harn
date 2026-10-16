//! The per-user root sets the sandbox grants beyond the workspace.
//!
//! These are data, not policy: each function answers "which directories does
//! this class of tooling keep under a user's home", and the preset-gated
//! wrappers in the parent module decide whether a given policy gets them. Both
//! the OS backends and the pure path-scope checks read the same answers from
//! here, which is what keeps a backend's rendered grant and the parent's view
//! of the jail from drifting apart.
//!
//! [`normalized_read_only_roots`] is the one policy reading here: the
//! read-only scope both consumers resolve. Host-granted external roots arrive
//! through `CapabilityPolicy::read_only_roots`; approval-policy metadata does
//! not independently grant filesystem access.

use std::path::{Path, PathBuf};

use super::paths::normalize_for_policy;
use crate::orchestration::CapabilityPolicy;

/// Normalize the policy's read-only roots. Unlike
/// [`super::normalized_workspace_roots`], an empty list stays empty — read-only
/// scope is purely additive, so there is no execution-root fallback to
/// synthesize.
///
/// Host-granted external roots are already projected into the capability
/// policy's `read_only_roots`. Keeping this scope owned by the capability
/// policy means a caller-authored approval policy cannot widen filesystem
/// access. Both the in-process filesystem builtins
/// ([`super::check_fs_path_scope`]) and the OS sandbox profile for a confined
/// child resolve the same roots here.
pub(super) fn normalized_read_only_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = normalized_declared_read_only_roots(policy);
    if super::declared_path_roots_active() {
        return roots;
    }
    // Object stores borrowed through `objects/info/alternates` (e.g. a
    // `git clone --shared`) live outside the workspace and are only ever read
    // by git; grant them read-only scope. See [`crate::stdlib::git_topology`].
    for dir in super::git_scope_extension_for_roots(&super::base_workspace_roots(policy)).read_only
    {
        if !roots.iter().any(|existing| existing == &dir) {
            roots.push(dir);
        }
    }
    roots
}

pub(super) fn normalized_declared_read_only_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in &policy.read_only_roots {
        let root = normalize_for_policy(&super::resolve_policy_path(root));
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn sandbox_user_home_dir() -> Option<PathBuf> {
    // Only an absolute home grounds the user-scope read-roots below; a
    // relative or unset home yields no extra roots (the safe direction).
    crate::user_dirs::home_dir().filter(|path| path.is_absolute())
}

/// Home roots for a preset class of the `crate::known_paths` registry,
/// resolved against `home`, sorted and deduplicated.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn home_roots<'a>(home: &Path, entries: impl IntoIterator<Item = &'a str>) -> Vec<PathBuf> {
    let mut roots: Vec<_> = entries
        .into_iter()
        .map(|entry| normalize_for_policy(&home.join(entry)))
        .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// Read-only DeveloperToolchains roots (`toolchain_read` in the registry).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn developer_toolchain_read_roots_for_home(home: &Path) -> Vec<PathBuf> {
    home_roots(
        home,
        crate::known_paths::toolchain_read_home_paths(cfg!(target_os = "windows")),
    )
}

/// Per-user toolchain cache roots (`toolchain_cache` in the registry) that
/// build tools read **and write** while a sandboxed build runs. Gated on the
/// `DeveloperToolchains` preset and granted write only when the active policy
/// lets its children write (`CapabilityPolicy::children_may_write`); otherwise
/// they fall back to read access so dependency resolution still works.
// Only the macOS (seatbelt) and Linux (Landlock) backends consume these; the
// Windows backend does not grant recursive home-scoped cache roots, and the
// cfg keeps `-D warnings` happy there.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn developer_toolchain_cache_write_roots_for_home(home: &Path) -> Vec<PathBuf> {
    home_roots(
        home,
        crate::known_paths::toolchain_cache_home_paths()
            .iter()
            .map(String::as_str),
    )
}

/// Read-only PackageManagerConfig roots (`package_config` in the registry).
/// No credential is among them; the registry refuses one at load.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn package_manager_config_read_roots_for_home(home: &Path) -> Vec<PathBuf> {
    home_roots(
        home,
        crate::known_paths::package_config_home_paths()
            .iter()
            .map(String::as_str),
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod path_grants;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn package_manager_roots_do_not_admit_unknown_xdg_siblings() {
        let home = tempfile::tempdir().unwrap();
        let roots = package_manager_config_read_roots_for_home(home.path());
        for relative in [".config/unlisted/token", ".cache/unlisted/token"] {
            let target = normalize_for_policy(&home.path().join(relative));
            assert!(
                !roots.iter().any(|root| target.starts_with(root)),
                "unknown XDG sibling is covered by a preset: {roots:?}"
            );
        }
        for relative in [".config/git", ".config/rustfmt", ".cache/pip"] {
            assert!(roots.contains(&normalize_for_policy(&home.path().join(relative))));
        }
    }

    #[test]
    fn read_only_xdg_roots_do_not_cancel_writable_toolchain_caches() {
        let home = tempfile::tempdir().unwrap();
        let reads = package_manager_config_read_roots_for_home(home.path());
        let cache = normalize_for_policy(&home.path().join(".cache"));
        let mut checked = 0;
        for writable in developer_toolchain_cache_write_roots_for_home(home.path()) {
            if writable.starts_with(&cache) {
                checked += 1;
                assert!(
                    !reads.iter().any(|read| writable.starts_with(read)),
                    "read-only {reads:?} would cancel writes to {writable:?} on macOS"
                );
            }
        }
        assert!(checked >= 2, "must check the Go and Harn writable caches");
    }
}
