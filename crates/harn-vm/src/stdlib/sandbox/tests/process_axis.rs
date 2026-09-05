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
fn a_read_only_root_is_launchable_without_becoming_writable() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let read_only = dir.path().join("persona");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&read_only).unwrap();

    let policy = CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        read_only_roots: vec![read_only.display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    };

    enforce_process_cwd_for_policy(&read_only, &policy)
        .expect("a read-only root is somewhere a subprocess may start");

    push_execution_policy(policy);
    let read = check_fs_path_scope(&read_only.join("persona.md"), FsAccess::Read);
    let write = check_fs_path_scope(&read_only.join("out.txt"), FsAccess::Write);
    pop_execution_policy();
    assert!(read.is_ok(), "file builtins retain declared read authority");
    assert!(
        write.is_err(),
        "making a read-only root launchable must not make it writable"
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
            ..Default::default()
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

/// The actual seam behind the agent's `run` tool (harn-hostlib's
/// `tools::run_command` -> `process::real::prepare_command` ->
/// `std_command_for`) with no sandbox policy and no session environment
/// active — exactly the shape `execute_playground_inputs` runs under
/// (harn#7993: a bare `node` failed to resolve here on Windows even though
/// the same policy resolved it fine through `harness.process.exec`'s own
/// `Inherited`-policy path). This pins that `std_command_for` itself hands
/// `Command::new` an absolute path instead of leaving PATH resolution to the
/// OS at spawn time.
#[test]
fn std_command_for_resolves_a_bare_program_name_to_an_absolute_path() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let program_path = dir.path().join("myfakeprogram");
    std::fs::write(&program_path, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", dir.path());
    let command = super::super::std_command_for("myfakeprogram", &[]);
    match original_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    let command = command.unwrap();
    assert_eq!(command.get_program(), program_path.as_os_str());
}

/// The bug this seam is defending against, made concrete: an Isolated
/// session whose child env carries no `PATH` at all (empty launcher
/// snapshot) must still resolve a bare name, because resolution reads THIS
/// process's own live `PATH`, never the child-shaped one — if the child's
/// `PATH` were the broken half, resolving against it would just reproduce
/// the failure instead of working around it.
#[test]
fn std_command_for_resolves_via_the_parent_path_even_when_the_isolated_child_env_has_none() {
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let program_path = dir.path().join("myfakeprogram2");
    std::fs::write(&program_path, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", dir.path());

    let isolated = crate::security::SessionEnvironment::launch_from_snapshot(
        crate::security::EnvironmentPolicyKind::Isolated,
        Vec::new(),
        std::collections::BTreeMap::new(), // empty: the child sees no PATH at all
        &|_| None,
    )
    .unwrap();
    crate::stdlib::process::set_session_environment(Some(isolated));

    let command = super::super::std_command_for("myfakeprogram2", &[]);

    crate::stdlib::process::set_session_environment(None);
    match original_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }

    let command = command.unwrap();
    assert_eq!(command.get_program(), program_path.as_os_str());
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
            ..Default::default()
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
