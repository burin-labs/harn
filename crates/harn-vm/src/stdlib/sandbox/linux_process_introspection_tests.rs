//! Process introspection grants and the kernel containment they require.

use super::*;

/// A host procfs rule must refuse a grant wider than process self-introspection.
#[test]
fn process_self_introspection_refuses_a_host_that_cannot_contain_it() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    policy.side_effect_level = Some("process_exec".to_string());
    *policy.process_sandbox = crate::orchestration::ProcessSandboxPolicy {
        allow_process_self_introspection: true,
        ..Default::default()
    };
    let refusal = landlock_profile("/bin/ls", &policy, SandboxProfile::Worktree)
        .err()
        .map(|error| format!("{error:?}"));
    if !landlock_available() {
        assert!(
            refusal.is_some(),
            "the Landlock renderer must refuse an unavailable boundary"
        );
    } else if proc_runtime_reads_are_contained() {
        assert!(
            refusal.is_none(),
            "a containing host must render the grant rather than refuse it: {refusal:?}",
        );
    } else {
        let refusal = refusal.expect(
            "a host that cannot contain a task's view of its neighbours must refuse \
             the grant instead of issuing a wider one",
        );
        assert!(
            refusal.contains("neighbours"),
            "the refusal must name why, not just fail: {refusal}",
        );
    }
}

#[test]
fn proc_runtime_reads_require_restricted_yama_scope() {
    for safe in ["1", "2\n", "3"] {
        assert!(yama_scope_contains_process_reads(safe), "scope {safe}");
    }
    for unsafe_or_unknown in ["0", "", "disabled", "256"] {
        assert!(
            !yama_scope_contains_process_reads(unsafe_or_unknown),
            "scope {unsafe_or_unknown} must not grant procfs reads",
        );
    }
}

/// The same real procfs enumeration must fire only with the grant.
#[test]
fn process_self_introspection_grant_controls_live_procfs_enumeration() {
    let _guard = handler_sandbox_test_guard();
    let lsm = active_lsm_list();
    match landlock_gate(
        LiveLandlock::probe(),
        live_landlock_required(),
        "process_self_introspection_grant_controls_live_procfs_enumeration",
        &lsm,
    ) {
        LandlockGate::Proceed => {}
        LandlockGate::Skip(reason) => {
            eprintln!("{reason}");
            return;
        }
        LandlockGate::Fail(reason) => panic!("{reason}"),
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let args = vec!["/proc/self/task".to_string()];
    let run_probe = |policy: &CapabilityPolicy| {
        let mut command = Command::new("/bin/ls");
        command.args(&args).current_dir(workspace.path());
        let preparation = Backend::prepare_std_command(
            "/bin/ls",
            &args,
            &mut command,
            policy,
            SandboxProfile::Worktree,
        )
        .expect("prepare sandboxed child");
        assert!(matches!(preparation, PrepareOutcome::Direct));
        command.output().expect("run sandboxed child")
    };
    let mut withheld = linux_policy_with_workspace_ops(&["read_text"]);
    withheld.workspace_roots = vec![workspace.path().display().to_string()];
    withheld.side_effect_level = Some("process_exec".to_string());
    let withheld_output = run_probe(&withheld);
    assert!(
        !withheld_output.status.success(),
        "procfs enumeration must be denied without the grant, otherwise the \
         grant is measuring nothing: {}",
        String::from_utf8_lossy(&withheld_output.stderr),
    );
    let mut granted = withheld;
    *granted.process_sandbox = crate::orchestration::ProcessSandboxPolicy {
        allow_process_self_introspection: true,
        ..Default::default()
    };
    let granted_output = run_probe(&granted);
    assert!(
        granted_output.status.success(),
        "the grant must let a child enumerate its own procfs entries: {}",
        String::from_utf8_lossy(&granted_output.stderr),
    );
}
