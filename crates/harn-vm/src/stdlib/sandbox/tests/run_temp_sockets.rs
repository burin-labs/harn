use super::*;

fn private_policy(workspace: &std::path::Path) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.display().to_string()],
        ..Default::default()
    };
    policy.process_sandbox.allow_run_temp_sockets = true;
    policy
}

#[test]
fn run_temp_sockets_refuse_missing_execution_ownership_before_command_preparation() {
    crate::runtime_stack::spawn(|| {
        let workspace = tempfile::tempdir().unwrap();
        let policy = private_policy(workspace.path());
        assert!(crate::current_execution_scope().is_none());
        assert!(workspace_local_tmpdir(&policy).is_none());
        push_execution_policy(policy.clone());
        let error = crate::stdlib::fs::workspace_temp_root().unwrap_err();
        pop_execution_policy();
        assert!(
            error.to_string().contains("active Harn execution scope"),
            "{error}"
        );
        assert_eq!(
            unix_socket_enforcement(&policy),
            UnixSocketEnforcement::Refused
        );
        push_execution_policy(policy);
        let error = std_command_for("not-executed", &[]).unwrap_err();
        pop_execution_policy();
        assert!(
            error.to_string().contains("active Harn execution scope"),
            "{error}"
        );
        assert!(!workspace.path().join(WORKSPACE_TMPDIR_NAME).exists());
    })
    .join()
    .unwrap();
}

#[test]
fn run_temp_sockets_select_one_directory_per_execution_without_granting_the_shared_parent() {
    let workspace = tempfile::tempdir().unwrap();
    let policy = private_policy(workspace.path());
    let shared = normalize_for_policy(workspace.path()).join(WORKSPACE_TMPDIR_NAME);
    let first_owner = crate::ExecutionId::mint();
    let first = {
        let _scope = crate::enter_execution_scope(first_owner.clone());
        let first = workspace_local_tmpdir(&policy).unwrap();
        assert_eq!(first, shared.join(first_owner.as_str()));
        assert_eq!(workspace_local_tmpdir(&policy).unwrap(), first);
        assert_eq!(
            process_sandbox_unix_socket_roots(&policy),
            vec![first.clone()]
        );
        assert!(first.is_dir());
        first
    };
    let second_owner = crate::ExecutionId::mint();
    let _scope = crate::enter_execution_scope(second_owner.clone());
    let second = workspace_local_tmpdir(&policy).unwrap();
    assert_eq!(second, shared.join(second_owner.as_str()));
    assert_ne!(first, second);
    let roots = process_sandbox_unix_socket_roots(&policy);
    assert_eq!(roots, vec![second]);
    assert!(!roots.contains(&shared));
    assert!(!roots.contains(&first));
    assert_eq!(
        std::fs::read_to_string(shared.join(".gitignore")).unwrap(),
        crate::runtime_paths::SELF_IGNORE_CONTENTS
    );
}

#[tokio::test]
async fn run_temp_sockets_keep_execution_ownership_across_interleaved_children() {
    fn child<F: std::future::Future>(
        owner: crate::ExecutionId,
        inner: F,
    ) -> impl std::future::Future<Output = F::Output> {
        let _scope = crate::enter_execution_scope(owner);
        crate::orchestration::scope_ambient_context(inner)
    }
    let workspace = tempfile::tempdir().unwrap();
    let policy = private_policy(workspace.path());
    let first = crate::ExecutionId::mint();
    let second = crate::ExecutionId::mint();
    let observe = || async {
        let before = workspace_local_tmpdir(&policy).unwrap();
        tokio::task::yield_now().await;
        let nested = crate::orchestration::scope_ambient_context(async {
            tokio::task::yield_now().await;
            workspace_local_tmpdir(&policy).unwrap()
        })
        .await;
        assert_eq!(before, nested);
        assert_eq!(workspace_local_tmpdir(&policy).unwrap(), before);
        before
    };
    let (a, b) = tokio::join!(
        child(first.clone(), observe()),
        child(second.clone(), observe())
    );
    assert!(a.ends_with(first.as_str()));
    assert!(b.ends_with(second.as_str()));
    assert_ne!(a, b);
}

#[cfg(unix)]
#[test]
fn run_temp_sockets_refuse_planted_parent_and_execution_symlinks() {
    for parent in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let policy = private_policy(workspace.path());
        let owner = crate::ExecutionId::mint();
        let _scope = crate::enter_execution_scope(owner.clone());
        let shared = workspace.path().join(WORKSPACE_TMPDIR_NAME);
        let planted = if parent {
            shared
        } else {
            std::fs::create_dir(&shared).unwrap();
            shared.join(owner.as_str())
        };
        std::os::unix::fs::symlink(outside.path(), &planted).unwrap();
        assert!(workspace_local_tmpdir(&policy).is_none());
        assert!(super::super::workspace_env::run_temp_directory(&policy).is_err());
        push_execution_policy(policy.clone());
        assert!(crate::stdlib::fs::workspace_temp_root().is_err());
        pop_execution_policy();
        assert!(process_sandbox_unix_socket_roots(&policy).is_empty());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
