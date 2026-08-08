//! Write-root confinement for conformance cases.
//!
//! The runner already gives every case a fresh `HARN_STATE_DIR`, but that is a
//! redirect, not a jail: it relies on each builtin resolving through the state
//! root. A case that names a path some other way — a relative path resolved
//! against the runner's working directory, an absolute path in a fixture — is
//! outside the redirect entirely, and the escape only ever surfaced as a dirty
//! worktree, and only when it happened to land inside the repository.
//!
//! This module turns that convention into an enforced boundary by scoping each
//! case future with a [`CapabilityPolicy`] whose writable roots are exactly the
//! directories a case owns. Reads are unrestricted: cases legitimately read
//! the repository, the stdlib, and their own fixtures, and the failure mode
//! being closed here is a stray *write*.

use std::path::{Path, PathBuf};

use harn_vm::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};

/// Build the capability ceiling for one conformance case.
///
/// `case_root` is the unique directory the runner just created. It is the sole
/// writable root; runtime state and host/workspace temp projections all derive
/// paths beneath it.
///
/// The case's own fixture directory is deliberately **not** writable. Cases
/// build scratch through case-owned workspace or host temp projections.
/// Granting the fixture directory would make accidental source-tree writes
/// valid by policy.
///
/// The source-execution boundary installs the policy inside its VM-owned
/// `LocalSet`, so the boundary remains attached to the case across awaits and
/// executor migration.
pub(crate) fn policy(case_root: &Path) -> CapabilityPolicy {
    case_policy(case_root)
}

/// Resolve the one root that all case-owned path projections derive from.
///
/// This is security-relevant normalization, not presentation cleanup. macOS
/// commonly creates a temp directory through `/var` and resolves descendants
/// through `/private/var`; intersecting those unnormalized aliases would treat
/// the same directory as two disjoint authority roots.
pub(crate) fn normalize_owned_root(case_root: &Path) -> std::io::Result<PathBuf> {
    case_root.canonicalize()
}

fn case_policy(case_root: &Path) -> CapabilityPolicy {
    let mut process_roots = runner_roots();
    process_roots.push(root_string(case_root));
    process_roots.sort();
    process_roots.dedup();

    CapabilityPolicy {
        workspace_roots: vec![root_string(case_root)],
        read_only_roots: filesystem_roots(),
        // A case may still launch a subprocess from the runner's directory.
        // That is the launch-cwd check, which is part of the path axis: it
        // reads `workspace_roots` plus these process-only roots. Without them
        // a case that shells out would be refused a working directory, since
        // the runner's cwd is deliberately not writable above.
        process_sandbox: ProcessSandboxPolicy {
            read_roots: process_roots.clone(),
            write_roots: process_roots,
            ..ProcessSandboxPolicy::default()
        },
        // Confine what a case *writes*, not what it *spawns*.
        //
        // Conformance cases are first-party code in this repository, and
        // several legitimately shell out — to `harn` itself, to a signal
        // driver, to an orchestrator. OS confinement buys nothing against
        // code we already trust, and costs a great deal: under `Worktree`
        // the platform sandbox denied five of those cases outright. The
        // failure this module exists to close is a stray *write*, which is
        // the path axis, and that is exactly what this rung enforces.
        sandbox_profile: SandboxProfile::WorkspacePaths,
        ..CapabilityPolicy::default()
    }
}

/// The runner's own working directory — the repository, in every normal run.
fn runner_roots() -> Vec<String> {
    std::env::current_dir()
        .map(|dir| vec![root_string(&dir)])
        .unwrap_or_default()
}

/// The filesystem roots a case may read from, as absolute prefixes.
///
/// Reads are deliberately unrestricted, so this is "wherever the case could
/// be reading from" rather than a curated list: the repository, the toolchain,
/// the temp dir and the state dir all live under one of these. Derived from
/// real anchors rather than a hardcoded `/` so a Windows run yields the drive
/// prefixes actually in play instead of a root that matches nothing.
fn filesystem_roots() -> Vec<String> {
    let anchors = [
        std::env::current_dir().ok(),
        Some(std::env::temp_dir()),
        std::env::current_exe().ok(),
    ];
    let mut roots: Vec<String> = anchors
        .into_iter()
        .flatten()
        .filter_map(|path| path.ancestors().last().map(Path::to_path_buf))
        .map(|root| root.display().to_string())
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

fn root_string(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_case_owns_its_root_but_not_its_fixture_directory() {
        let policy = case_policy(Path::new("/tmp/harn-conformance-case-x"));

        assert!(policy
            .workspace_roots
            .iter()
            .any(|root| root == "/tmp/harn-conformance-case-x"));
        // Cases build scratch under their owned root, never beside themselves.
        assert!(
            !policy
                .workspace_roots
                .iter()
                .any(|root| root.contains("conformance/tests")),
            "a fixture directory is not the case's to write: {:?}",
            policy.workspace_roots
        );
    }

    #[test]
    fn a_checkout_below_the_host_temp_root_is_not_writable() {
        let host_temp = tempfile::tempdir().expect("host temp root");
        let case_root = host_temp.path().join("case-root");
        let checkout = host_temp.path().join("runner-checkout");
        let policy = case_policy(&case_root);

        assert!(
            policy
                .workspace_roots
                .iter()
                .all(|root| !checkout.starts_with(Path::new(root))),
            "an ambient temp root must not grant an unrelated checkout: {:?}",
            policy.workspace_roots
        );
    }

    #[test]
    fn owned_root_is_normalized_before_it_becomes_authority() {
        let host_temp = tempfile::tempdir().expect("host temp root");
        let case_root = host_temp.path().join("case-root");
        let alias_parent = host_temp.path().join("alias-parent");
        std::fs::create_dir_all(&case_root).expect("case root");
        std::fs::create_dir_all(&alias_parent).expect("alias parent");
        let aliased = alias_parent.join("..").join("case-root");

        assert_eq!(
            normalize_owned_root(&aliased).expect("normalize owned root"),
            case_root.canonicalize().expect("canonical case root")
        );
    }

    #[test]
    fn reads_are_not_confined() {
        let policy = case_policy(Path::new("/state/.harn"));
        assert!(
            !policy.read_only_roots.is_empty(),
            "a case must still be able to read the repository and the toolchain"
        );
        for root in &policy.read_only_roots {
            assert_eq!(
                Path::new(root).ancestors().last().map(Path::to_path_buf),
                Some(PathBuf::from(root)),
                "read roots are filesystem roots, not curated directories"
            );
        }
    }

    #[test]
    fn subprocesses_may_still_start_from_the_runners_directory() {
        let case_root = Path::new("/case-root");
        let policy = case_policy(case_root);
        let cwd = root_string(&std::env::current_dir().unwrap());
        assert!(
            policy.process_sandbox.read_roots.contains(&cwd)
                && policy.process_sandbox.write_roots.contains(&cwd),
            "cases shell out from the runner's directory: {:?}",
            policy.process_sandbox
        );
        let case_root = root_string(case_root);
        assert!(
            policy.process_sandbox.read_roots.contains(&case_root)
                && policy.process_sandbox.write_roots.contains(&case_root),
            "cases shell out from their owned scratch root: {:?}",
            policy.process_sandbox
        );
        assert!(
            !policy.workspace_roots.contains(&cwd),
            "the runner's directory is launchable, not writable: {:?}",
            policy.workspace_roots
        );
    }

    #[test]
    fn the_profile_confines_writes_but_not_subprocesses() {
        let profile = case_policy(Path::new("/state/.harn")).sandbox_profile;
        assert!(
            profile.enforces_path_scope(),
            "a stray write is the failure this module exists to catch"
        );
        assert!(
            !profile.confines_processes(),
            "cases legitimately shell out; OS confinement would deny them \
             without closing the write hole"
        );
    }
}
