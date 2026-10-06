use super::*;

#[test]
fn declared_path_root_selection_restores_after_nested_error_and_panic() {
    assert!(!declared_path_roots_active());
    let failure: Result<(), ()> = with_declared_path_roots(|| {
        assert!(declared_path_roots_active());
        let nested: Result<(), ()> = with_declared_path_roots(|| Err(()));
        assert!(nested.is_err());
        assert!(declared_path_roots_active());
        assert!(std::panic::catch_unwind(|| with_declared_path_roots(|| {
            assert!(declared_path_roots_active());
            panic!("nested root-selection restoration control");
        }))
        .is_err());
        assert!(declared_path_roots_active());
        Err(())
    });
    assert!(failure.is_err());
    assert!(!declared_path_roots_active());
    assert!(std::panic::catch_unwind(|| with_declared_path_roots(|| {
        assert!(declared_path_roots_active());
        panic!("root-selection restoration control");
    }))
    .is_err());
    assert!(!declared_path_roots_active());
}

#[test]
fn declared_child_failure_restores_execution_policy_and_root_selection() {
    struct PolicyGuard;
    impl Drop for PolicyGuard {
        fn drop(&mut self) {
            pop_execution_policy();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![root.path().display().to_string()],
        ..CapabilityPolicy::default()
    };
    let original_presets = policy.process_sandbox.effective_presets();
    push_execution_policy(policy);
    let _policy = PolicyGuard;
    assert!(command_output_with_declared_roots(
        "",
        &[],
        &ProcessCommandConfig {
            cwd: Some(root.path().to_path_buf()),
            ..Default::default()
        }
    )
    .is_err());
    assert!(!declared_path_roots_active());
    let restored = crate::orchestration::current_execution_policy().unwrap();
    assert_eq!(
        restored.process_sandbox.effective_presets(),
        original_presets
    );
    assert_eq!(
        restored.workspace_roots,
        vec![root.path().display().to_string()]
    );
}
