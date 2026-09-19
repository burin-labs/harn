//! The public finalization path and durable run projection must name one cause.

use harn_session_store::SessionType;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn canonical_terminal_causes_survive_the_durable_projection() {
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().expect("temporary session root");
    let root_literal =
        serde_json::to_string(root.path().to_str().expect("UTF-8 path")).expect("serialize root");
    let source = include_str!(
        "../../../conformance/tests/mechanisms/terminal_cause_consistency.contract.harn"
    )
    .replace(
        "harness.fs.mkdtemp_in_workspace(\"terminal-cause\")",
        &root_literal,
    );
    let chunk = crate::compile_source(&source).expect("compile public finalization fixture");
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk)
                .await
                .expect("run finalization and both controls");
            assert!(vm.output().contains("clean completion: natural"));
        })
        .await;

    for (session, expected_kind, expected_status) in [
        ("conflicting-terminal", "unknown", "failed"),
        ("budget-terminal", "policy_budget", "stopped"),
        ("natural-terminal", "natural", "completed"),
    ] {
        let store = crate::stdlib::session_store::open_canonical_agent_session(
            &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
            session,
            None,
            SessionType::User,
        )
        .await
        .expect("open persisted session");
        let run = crate::orchestration::project_run_record_from_session(&store, session)
            .await
            .expect("project sealed run");
        let terminal = &run.metadata["terminal"];
        assert_eq!(terminal["kind"], expected_kind, "{session}");
        assert_eq!(run.status, expected_status, "{session}");
        assert_eq!(run.metadata["stop_reason"], terminal["reason"], "{session}");
        assert!(!run.metadata.contains_key("terminal_class"), "{session}");
        if session == "conflicting-terminal" {
            assert_eq!(terminal["reason"], "conflicting_terminal_evidence");
            assert_eq!(
                run.metadata["terminal_error"],
                json!({
                    "category": "fixture_failure",
                    "message": "Recorded failure without a proven cause",
                }),
            );
        }
    }
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
}
