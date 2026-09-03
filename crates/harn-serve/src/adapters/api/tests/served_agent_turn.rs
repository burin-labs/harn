use super::*;

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

async fn assert_advertised_task_controls(app: &Router) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/capabilities")
                .body(Body::empty())
                .expect("capabilities request"),
        )
        .await
        .expect("capabilities response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("capabilities body");
    let summary: Value = serde_json::from_slice(&body).expect("capabilities json");
    assert!(
        summary["capabilities"]
            .as_array()
            .expect("capability list")
            .iter()
            .any(|entry| entry["id"] == "tasks"),
        "the advertised API surface must include task submission and cancellation: {summary}"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/tools")
                .body(Body::empty())
                .expect("tools request"),
        )
        .await
        .expect("tools response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("tools body");
    let tools: Value = serde_json::from_slice(&body).expect("tools json");
    let advertised = tools["data"].as_array().expect("tool list");
    for tool_id in ["harn.session.prompt", "harn.session.cancel"] {
        assert!(
            advertised.iter().any(|entry| entry["id"] == tool_id),
            "advertised task control {tool_id} is missing: {tools}"
        );
    }
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

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.expect("task terminal event");
            if event.task_id.as_deref() == Some(task_id)
                && matches!(event.event.as_str(), "task.completed" | "task.failed")
            {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for terminal event for task {task_id}"))
}

/// The Agents API route that reported the original failure, plus a negative
/// control proving the default session still carries its read-only ceiling.
#[tokio::test]
async fn default_agents_api_task_reaches_a_model_turn_and_refuses_a_workspace_write() {
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
    // Pin the workspace root explicitly. `for_pipeline` intentionally honors
    // the process-wide `HARN_PROJECT_ROOT`; allowing ambient test state to
    // redirect this root would make the file-absence control below vacuous.
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().to_string());
    config.workspace_root = dir.path().to_path_buf();
    let server = ApiServer::new(config);
    let state = server.state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);

    assert_advertised_task_controls(&app).await;

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
    assert!(
        !dir.path().join("ceiling-probe.txt").exists(),
        "the rejected workspace write reached the filesystem"
    );
}
