//! A completion judge spends on its session's behalf, so the durable run record
//! must bill it beside the agent's own turns (harn#9400).

use harn_session_store::SessionType;

#[tokio::test(flavor = "current_thread")]
async fn completion_judge_calls_are_billed_in_the_durable_run_record() {
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().expect("temporary session root");
    let root_literal =
        serde_json::to_string(root.path().to_str().expect("UTF-8 path")).expect("serialize root");
    // Read the fixture at run time: a packaged crate cannot `include_str!` a
    // path outside its own directory. `expect` keeps a missing fixture loud.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/tests/agents/agent_loop_turn_end_judge_directive_isolation.harn");
    let source = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|error| panic!("read {}: {error}", fixture.display()))
        .replace(
            "harness.fs.mkdtemp_in_workspace(\"judge-directives\")",
            &root_literal,
        );
    let chunk = crate::compile_source(&source).expect("compile judge fixture");
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk).await.expect("run the judged agent loop");
            assert!(
                vm.output()
                    .contains("agent.main,completion.judge,agent.main,completion.judge"),
                "the fixture must reach both judge calls: {}",
                vm.output()
            );
        })
        .await;

    let session = "turn-end-judge-directives";
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
        .expect("project the judged run");
    let usage = run.usage.expect("the run record carries usage");
    assert_eq!(
        usage.call_count, 4,
        "two agent turns and two judge calls must all be billed: {usage:?}"
    );
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
}
