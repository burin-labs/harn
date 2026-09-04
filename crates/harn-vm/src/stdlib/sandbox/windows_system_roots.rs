//! Windows system read roots: the directories a confined child needs in
//! order to run the toolchains the parent process can already run.
//!
//! The product contract is reads-open, writes-confined. Every other backend
//! meets it by denying writes and leaving reads alone. The Windows backend
//! cannot: an AppContainer child reads a file only when that file's DACL
//! admits the container SID, a capability SID the token carries, or
//! `ALL APPLICATION PACKAGES` (`S-1-15-2-1`), which every AppContainer token
//! carries. Anything the host installed with an ACL that omits all three is
//! invisible to the child, and `cmd.exe` reports an unreadable executable as
//! "'x' is not recognized as an internal or external command" — a message
//! that reads as a PATH defect and is not one.
//!
//! This module owns the *set* of roots that gap covers: every existing
//! directory on the parent's `PATH`, plus the standard system prefixes and
//! the hosted tool cache when the environment names one. The Windows backend
//! is the only consumer.
//!
//! ## Why this set is not simply granted
//!
//! Granting read to the per-spawn container SID means `icacls /T` over the
//! root, because Windows inheritance is not dynamic: an inheritable ACE
//! placed on a directory does not reach the files already inside it. A
//! recursive ACL rewrite of `C:\Windows` or `C:\Program Files` is both slow
//! and a mutation of system state, so [`broad_system_root`] names the roots
//! that are never granted under any circumstance. The backend probes first
//! and grants only the narrow leaf roots that a probe proves are closed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::normalize_for_policy;

/// Environment variables that name the standard system prefixes. Read from
/// the environment rather than hard-coded so a non-`C:` system volume, a
/// relocated `ProgramData`, or a hosted runner's tool cache is covered.
const SYSTEM_ROOT_ENV_VARS: &[&str] = &[
    "SystemRoot",
    "windir",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    // Hosted CI images place their managed toolchains (node, python, go, …)
    // under a tool cache named by one of these; on a developer machine
    // neither is set and the entry is simply absent.
    "AGENT_TOOLSDIRECTORY",
    "RUNNER_TOOL_CACHE",
];

/// Every existing directory on the parent's `PATH`, plus the standard system
/// prefixes, normalized and deduplicated in `PATH` order (system prefixes
/// last). Non-existent and non-directory entries are dropped: a `PATH` entry
/// that does not resolve grants nothing and would only make a backend's
/// not-found handling ambiguous.
///
/// The result is cached for the life of the process. `PATH` is captured once
/// at startup by the OS and the system prefixes do not move, so re-reading
/// them per spawn would buy nothing and cost a syscall per entry on a `PATH`
/// that routinely runs past a hundred directories.
pub(crate) fn system_read_roots() -> Vec<PathBuf> {
    static ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(compute_system_read_roots).clone()
}

fn compute_system_read_roots() -> Vec<PathBuf> {
    let path_entries = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let system_entries = SYSTEM_ROOT_ENV_VARS
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from);
    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    for entry in path_entries.into_iter().chain(system_entries) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let normalized = normalize_for_policy(&entry);
        if !normalized.is_absolute() || !normalized.is_dir() {
            continue;
        }
        // Case-insensitive identity: `C:\Windows\System32` and
        // `C:\WINDOWS\system32` are one root, and a `PATH` that names both
        // must not produce two grants.
        let key = normalized.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            roots.push(normalized);
        }
    }
    roots
}

/// Roots that are never handed to a recursive ACL grant, whatever a probe
/// says about them: a drive root, the Windows directory, either Program
/// Files prefix, `ProgramData`, and the user's home. Rewriting the ACLs
/// under any of these takes minutes and mutates system state that outlives
/// the spawn if the process dies before its grants are removed.
///
/// A leaf under one of them (`C:\Program Files\nodejs`) is not broad; the
/// prefix itself is.
pub(crate) fn broad_system_root(path: &Path) -> bool {
    // A path with no parent, or whose only components are a drive prefix and
    // a root, is a volume root.
    let depth = path
        .components()
        .filter(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::ParentDir
            )
        })
        .count();
    if depth == 0 {
        return true;
    }
    let normalized = path.to_string_lossy().to_ascii_lowercase();
    let normalized = normalized.trim_end_matches(['\\', '/']).to_string();
    SYSTEM_ROOT_ENV_VARS
        .iter()
        .filter_map(std::env::var_os)
        .map(|value| {
            normalize_for_policy(Path::new(&value))
                .to_string_lossy()
                .to_ascii_lowercase()
                .trim_end_matches(['\\', '/'])
                .to_string()
        })
        .chain(crate::user_dirs::home_dir().map(|home| {
            normalize_for_policy(&home)
                .to_string_lossy()
                .to_ascii_lowercase()
                .trim_end_matches(['\\', '/'])
                .to_string()
        }))
        .any(|root| !root.is_empty() && root == normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_root_is_broad() {
        assert!(broad_system_root(Path::new("C:\\")));
        assert!(broad_system_root(Path::new("\\")));
    }

    #[test]
    fn a_leaf_under_a_system_prefix_is_not_broad() {
        // The prefix itself is broad only when the environment names it, so
        // this asserts the shape that never depends on the host: a two-deep
        // path under a prefix is a leaf, and leaves are grantable.
        assert!(!broad_system_root(Path::new("C:\\Program Files\\nodejs")));
    }

    #[test]
    fn system_read_roots_are_absolute_directories_without_duplicates() {
        let roots = system_read_roots();
        let mut seen = BTreeSet::new();
        for root in &roots {
            assert!(root.is_absolute(), "non-absolute read root {root:?}");
            assert!(root.is_dir(), "non-directory read root {root:?}");
            assert!(
                seen.insert(root.to_string_lossy().to_ascii_lowercase()),
                "duplicate read root {root:?}"
            );
        }
    }
}
