//! Workspace scope for Harn's own runtime directories.
//!
//! The runtime writes its state, run records, and managed worktrees from Rust
//! wherever `HARN_STATE_DIR` and friends point. These cover the matching rule
//! for `harness.fs.*`, so relocating those directories does not leave them
//! readable and unwritable from Harn code.

use std::path::{Path, PathBuf};

use super::super::{check_fs_path_scope, relocated_runtime_roots, FsAccess};
use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use crate::runtime_paths::{HARN_RUN_DIR_ENV, HARN_STATE_DIR_ENV};

/// Restores every runtime-root override on drop, so one test's relocation
/// cannot leak into another's view of where the state root lives.
struct RuntimeRootEnv {
    saved: Vec<(&'static str, Option<String>)>,
}

impl RuntimeRootEnv {
    fn set(overrides: &[(&'static str, &Path)]) -> Self {
        let keys = [HARN_STATE_DIR_ENV, HARN_RUN_DIR_ENV];
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        for (key, value) in overrides {
            std::env::set_var(key, value);
        }
        Self { saved }
    }
}

impl Drop for RuntimeRootEnv {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn normalized(path: &Path) -> PathBuf {
    super::super::normalize_for_policy(path)
}

#[test]
fn a_relocated_state_root_joins_the_workspace_scope() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let _env = RuntimeRootEnv::set(&[(HARN_STATE_DIR_ENV, elsewhere.path())]);

    let relocated = relocated_runtime_roots(&[normalized(workspace.path())]);

    assert!(
        relocated.contains(&normalized(elsewhere.path())),
        "a state root outside the workspace must be granted: {relocated:?}"
    );
}

#[test]
fn the_default_layout_grants_nothing_extra() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let _env = RuntimeRootEnv::set(&[]);
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    let relocated = relocated_runtime_roots(&[normalized(workspace.path())]);

    crate::stdlib::process::set_thread_execution_context(None);
    assert!(
        relocated.is_empty(),
        "`.harn` and `.harn-runs` already sit inside the workspace, so the \
         common case must not widen the jail at all: {relocated:?}"
    );
}

#[test]
fn a_worktree_root_nested_under_the_state_root_is_not_listed_twice() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let _env = RuntimeRootEnv::set(&[(HARN_STATE_DIR_ENV, elsewhere.path())]);
    // Pin the base so the run root resolves inside `workspace` rather than
    // wherever the test binary happens to be running from.
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    let relocated = relocated_runtime_roots(&[normalized(workspace.path())]);

    crate::stdlib::process::set_thread_execution_context(None);
    // `worktree_root` defaults to `<state_root>/worktrees`, which the state
    // root already covers. Listing it again would only pad the diagnostic.
    assert_eq!(
        relocated,
        vec![normalized(elsewhere.path())],
        "the default worktree root is covered by the state root"
    );
}

#[test]
fn no_granted_root_is_nested_inside_another() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let runs = state.path().join("nested-runs");
    std::fs::create_dir_all(&runs).unwrap();
    let _env = RuntimeRootEnv::set(&[
        (HARN_STATE_DIR_ENV, state.path()),
        (HARN_RUN_DIR_ENV, runs.as_path()),
    ]);

    let relocated = relocated_runtime_roots(&[normalized(workspace.path())]);

    // Even when an operator nests one override inside another, each granted
    // root must be distinct — a root already covered adds nothing but noise
    // to the `outside workspace_roots [...]` diagnostic.
    for (index, root) in relocated.iter().enumerate() {
        for (other_index, other) in relocated.iter().enumerate() {
            assert!(
                index == other_index || !super::super::path_is_within(root, other),
                "{root:?} is already covered by {other:?}: {relocated:?}"
            );
        }
    }
}

#[test]
fn relocation_grants_the_state_root_and_nothing_beside_it() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let state_root = parent.path().join("harn-state");
    std::fs::create_dir_all(&state_root).unwrap();
    let sibling = parent.path().join("not-harn-state");
    std::fs::create_dir_all(&sibling).unwrap();
    let _env = RuntimeRootEnv::set(&[(HARN_STATE_DIR_ENV, state_root.as_path())]);

    push_execution_policy(CapabilityPolicy {
        workspace_roots: vec![workspace.path().display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });
    let inside = check_fs_path_scope(&state_root.join("receipts"), FsAccess::Write);
    let beside = check_fs_path_scope(&sibling.join("loot"), FsAccess::Write);
    pop_execution_policy();

    assert!(
        inside.is_ok(),
        "the operator pointed Harn's state here: {inside:?}"
    );
    assert!(
        beside.is_err(),
        "granting the state root must not grant its parent directory"
    );
}
