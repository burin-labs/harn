use super::*;

struct MockModeGuard;

impl Drop for MockModeGuard {
    fn drop(&mut self) {
        harn_vm::llm::clear_cli_llm_mock_mode();
    }
}

async fn create_session(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace_id":"local"}"#))
                .expect("session request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("session body");
    let session: Value = serde_json::from_slice(&body).expect("session json");
    session["id"].as_str().expect("session id").to_string()
}

async fn submit_and_wait(
    app: &Router,
    events: &mut broadcast::Receiver<ApiEvent>,
    session_id: &str,
    prompt: &str,
) -> ApiEvent {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/tasks"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"input": prompt})).expect("task json"),
                ))
                .expect("task request"),
        )
        .await
        .expect("task response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("task body");
    let task: Value = serde_json::from_slice(&body).expect("task json");
    let task_id = task["id"].as_str().expect("task id");

    loop {
        let event = events.recv().await.expect("task terminal event");
        if event.task_id.as_deref() == Some(task_id)
            && matches!(event.event.as_str(), "task.completed" | "task.failed")
        {
            return event;
        }
    }
}

/// The Agents API route that reported the original failure, plus a negative
/// control proving the default session still carries its read-only ceiling.
#[tokio::test]
async fn default_agents_api_task_reaches_a_model_turn_and_refuses_a_workspace_write() {
    let mock = harn_vm::llm::parse_llm_mock_value(
        &json!({"text": "all done", "model": "served-proof", "provider": "mock"}),
    )
    .expect("mock fixture");
    harn_vm::llm::install_cli_llm_mocks(vec![mock]);
    let _mock_mode = MockModeGuard;

    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    std::fs::write(
        &script,
        r#"import { agent_loop } from "std/agent/loop"
pipeline main(harness: Harness) {
  if prompt == "runtime-control" {
    agent_loop(harness, prompt, nil, {provider: "mock", model: "served-proof"})
    assert(
      len(harness.llm.mock_calls()) == 1,
      "the default served task must consume exactly one model call",
    )
    harness.stdio.println("control-plane-ok")
  } else {
    harness.fs.write_text("ceiling-probe.txt", "must-not-write")
  }
}
"#,
    )
    .expect("write pipeline");
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let state = server.state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);

    let admitted_session = create_session(&app).await;
    let admitted = submit_and_wait(&app, &mut events, &admitted_session, "runtime-control").await;
    assert_eq!(
        admitted.event, "task.completed",
        "the API task must reach and finish runtime-owned session bookkeeping: {}",
        admitted.payload
    );

    let denied_session = create_session(&app).await;
    let denied = submit_and_wait(&app, &mut events, &denied_session, "workspace-write").await;
    assert_eq!(
        denied.event, "task.failed",
        "workspace write escaped ceiling"
    );
    let failure = denied.payload["failure"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        failure.contains("exceeds the active effect ceiling"),
        "the control must fail at the read-only ceiling, not for an unrelated reason: {failure}"
    );
}
