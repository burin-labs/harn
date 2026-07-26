//! Write-root confinement for conformance cases.
//!
//! The runner already gives every case a fresh `HARN_STATE_DIR`, but that is a
//! redirect, not a jail: it relies on each builtin resolving through the state
//! root. A case that names a path some other way — a relative path resolved
//! against the runner's working directory, an absolute path in a fixture — is
//! outside the redirect entirely, and the escape only ever surfaced as a dirty
//! worktree, and only when it happened to land inside the repository.
//!
//! This module turns that convention into an enforced boundary by pushing a
//! [`CapabilityPolicy`] whose writable roots are exactly the directories a
//! case owns. Reads are unrestricted: cases legitimately read the repository,
//! the stdlib, and their own fixtures, and the failure mode being closed here
//! is a stray *write*.

use std::path::Path;

use harn_vm::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};

/// Pops the case policy when the case finishes, whatever the outcome.
pub(crate) struct ConformanceWriteRoot;

impl Drop for ConformanceWriteRoot {
    fn drop(&mut self) {
        harn_vm::orchestration::pop_execution_policy();
    }
}

impl ConformanceWriteRoot {
    /// Confine the case to the directories it owns.
    ///
    /// `state_dir` is the per-case `.harn` the runner just created. The system
    /// temp directory is writable because a case that asks for a temp dir has
    /// already isolated itself.
    ///
    /// The case's own fixture directory is deliberately **not** writable. An
    /// earlier draft granted it, on the reasoning that cases legitimately build
    /// scratch next to themselves. That stopped being true: harn#5583, #5586,
    /// and #5589 moved every such case to `harness.fs.temp_dir()`. Granting it
    /// back would re-open by policy precisely the hole those changes closed by
    /// construction.
    ///
    /// Everything else — the repository included — is readable and unwritable,
    /// so a case that writes there fails with a `HARN-CAP-201` diagnostic
    /// naming the path.
    pub(crate) fn install(state_dir: &Path) -> Self {
        harn_vm::orchestration::push_execution_policy(case_policy(state_dir));
        Self
    }
}

fn case_policy(state_dir: &Path) -> CapabilityPolicy {
    let mut workspace_roots = vec![root_string(state_dir), root_string(&std::env::temp_dir())];
    workspace_roots.sort();
    workspace_roots.dedup();

    CapabilityPolicy {
        workspace_roots,
        read_only_roots: filesystem_roots(),
        // A case may still launch a subprocess from the runner's directory.
        // That is the launch-cwd check, which is part of the path axis: it
        // reads `workspace_roots` plus these process-only roots. Without them
        // a case that shells out would be refused a working directory, since
        // the runner's cwd is deliberately not writable above.
        process_sandbox: ProcessSandboxPolicy {
            read_roots: runner_roots(),
            write_roots: runner_roots(),
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
    fn the_case_owns_its_state_dir_but_not_its_fixture_directory() {
        let policy = case_policy(Path::new("/tmp/harn-conformance-state-x/.harn"));

        assert!(policy
            .workspace_roots
            .iter()
            .any(|root| root == "/tmp/harn-conformance-state-x/.harn"));
        // Cases build scratch in `temp_dir()`, never beside themselves.
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
        let policy = case_policy(Path::new("/state/.harn"));
        let cwd = root_string(&std::env::current_dir().unwrap());
        assert!(
            policy.process_sandbox.read_roots.contains(&cwd)
                && policy.process_sandbox.write_roots.contains(&cwd),
            "cases shell out from the runner's directory: {:?}",
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
