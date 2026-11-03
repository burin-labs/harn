use super::*;
use harn_session_store::task::TaskStatus;

#[tokio::test(flavor = "current_thread")]
async fn failed_agent_turn_is_a_failed_api_task_despite_successful_prompt_rpc() {
    harn_clock::test_support::with_local_tasks(async {
        let dir = tempfile::tempdir().expect("workspace fixture");
        let script = dir.path().join("agent.harn");
        std::fs::write(
            &script,
            r#"import { agent_loop } from "std/agent/loop"
pipeline main(harness: Harness) {
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({error: {status: 401, message: "served-terminal-credential-refused"}})
  agent_loop(harness, prompt, nil, {provider: "mock", model: "served-terminal"})
  assert(len(harness.llm.mock_calls()) == 1, "exact provider failure must fire")
}
"#,
        )
        .expect("served pipeline");
        let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy());
        config.workspace_root = dir.path().to_path_buf();
        let server = ApiServer::new(config);
        let state = server.state;
        let mut events = state.events_tx.subscribe();
        let app = api_router(state.clone());
        let response = post(
            &app,
            "/v1/sessions",
            json!({"workspace_id":"local", "mode_id":"code"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let session = read_json(response).await["id"].as_str().unwrap().to_owned();
        let terminal =
            served_agent_turn::submit_and_wait(&app, &mut events, &session, "explain the file")
                .await;
        assert_eq!(terminal.event, "task.failed", "{}", terminal.payload);
        let task_id = terminal.task_id.as_deref().expect("task identity");
        let task = state.task_store().unwrap().get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        let failure = task.failure.expect("durable failure diagnostic");
        assert!(
            failure
                .message
                .contains("served-terminal-credential-refused"),
            "{failure:?}"
        );
        assert!(state
            .task_store()
            .unwrap()
            .task_events(Some(task_id), None)
            .unwrap()
            .iter()
            .all(|event| event.event != "task.completed"));
        drop(dir);
    })
    .await;
}

#[test]
fn prompt_outcome_preserves_typed_terminal_and_rejects_malformed_evidence() {
    use harn_vm::agent_events::AgentTerminalKind as Kind;
    for (kind, status) in [
        (Kind::Natural, TaskStatus::Completed),
        (Kind::UserCancelled, TaskStatus::Canceled),
        (Kind::PolicyBudget, TaskStatus::Canceled),
        (Kind::RuntimeError, TaskStatus::Failed),
        (Kind::ProviderError, TaskStatus::Failed),
        (Kind::CompletionUnverified, TaskStatus::Failed),
        (Kind::Suspended, TaskStatus::Failed),
        (Kind::Unknown, TaskStatus::Failed),
    ] {
        let (actual, failure) = tasks::prompt_task_outcome(json!({
            "stopReason": "end_turn", "_meta": {"harn": {"terminal": {
                "kind": kind.as_str(), "owner": kind.owner(), "reason": "measured",
                "message": "exact diagnostic",
            }}}
        }));
        assert_eq!(actual, status, "{kind:?}");
        if status == TaskStatus::Failed {
            assert_eq!(failure.unwrap().message, "exact diagnostic");
        } else {
            assert!(failure.is_none());
        }
    }
    for value in [
        json!({}),
        json!({"stopReason":"unknown"}),
        json!({"stopReason":"end_turn","_meta":{"harn":{"terminal":{"kind":"new_kind"}}}}),
    ] {
        assert_eq!(tasks::prompt_task_outcome(value).0, TaskStatus::Failed);
    }
    assert_eq!(
        tasks::prompt_task_outcome(json!({"stopReason":"end_turn"})).0,
        TaskStatus::Completed
    );
}
