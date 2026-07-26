//! Tests for the process axis: which directories a child may be launched
//! from, and how that differs from what the file builtins may write.

use super::super::process_cwd::enforce_process_cwd_for_policy;
use super::super::*;
use crate::orchestration::{pop_execution_policy, push_execution_policy};

#[test]
fn a_process_only_root_is_launchable_without_becoming_writable() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();

    let policy = CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        process_sandbox: crate::orchestration::ProcessSandboxPolicy {
            read_roots: vec![elsewhere.display().to_string()],
            ..Default::default()
        },
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    };

    enforce_process_cwd_for_policy(&elsewhere, &policy)
        .expect("a process-only root is somewhere a subprocess may start");

    push_execution_policy(policy);
    let write = check_fs_path_scope(&elsewhere.join("out.txt"), FsAccess::Write);
    pop_execution_policy();
    assert!(
        write.is_err(),
        "granting a root on the process axis must not let file builtins write there"
    );
}

#[test]
fn a_root_on_neither_axis_is_not_launchable() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let policy = CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    };

    let error = enforce_process_cwd_for_policy(&outside, &policy)
        .expect_err("an ungranted directory is not a legal cwd");
    assert!(
        error.to_string().contains("process cwd"),
        "the diagnostic must say which axis rejected the launch: {error}"
    );
}

#[test]
fn empty_workspace_roots_default_to_execution_root_for_process_cwd() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::remove_var("HARN_PROJECT_ROOT");
    let dir = tempfile::tempdir().unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    assert!(enforce_process_cwd(dir.path()).is_ok());
    let outside = tempfile::tempdir().unwrap();
    assert!(enforce_process_cwd(outside.path()).is_err());

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
}

#[test]
fn scoped_process_sandbox_roots_concretize_empty_policy_for_command_cwd() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::remove_var("HARN_PROJECT_ROOT");
    let execution_root = tempfile::tempdir().unwrap();
    let command_root = tempfile::tempdir().unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(execution_root.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    assert!(
        enforce_process_cwd(command_root.path()).is_err(),
        "before the scoped overlay the command temp root is outside the execution-root fallback",
    );
    {
        let _guard = push_process_sandbox_scope(ProcessSandboxScope {
            workspace_roots: vec![command_root.path().to_string_lossy().into_owned()],
        })
        .unwrap();
        assert!(
            enforce_process_cwd(command_root.path()).is_ok(),
            "scoped command root must be usable as the process cwd"
        );
        assert!(
            enforce_process_cwd(execution_root.path()).is_err(),
            "the scoped root must narrow the concrete spawn jail instead of widening it"
        );
    }
    assert!(
        enforce_process_cwd(command_root.path()).is_err(),
        "the scoped command root must pop after the command spawn"
    );

    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
}
