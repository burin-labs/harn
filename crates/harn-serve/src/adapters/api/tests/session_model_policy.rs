use super::*;

#[tokio::test]
async fn round_trips_and_reaches_execution() {
    struct MockModeGuard;
    impl Drop for MockModeGuard {
        fn drop(&mut self) {
            harn_vm::llm::clear_cli_llm_mock_mode();
        }
    }

    let mock = harn_vm::llm::parse_llm_mock_value(
        &json!({"text": "ok", "model": "o3", "provider": "mock"}),
    )
    .expect("mock fixture");
    harn_vm::llm::install_cli_llm_mocks(vec![mock]);
    let _mock_mode = MockModeGuard;

    let (_dir, server) = pipeline_server(
        r#"pipeline main(harness: Harness) {
  harness.llm.call(prompt, nil, {provider: "mock", model: "o3"})
  const call = harness.llm.mock_calls()[0]
  assert(
    call.thinking.mode == "effort",
    "session reasoning mode reached provider: ${json_stringify(call.thinking)}",
  )
  assert(call.thinking.level == "high", "session reasoning effort reached provider")
  harness.stdio.println("session-model-policy-fired")
}
"#,
    );
    let state = server.state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);

    let response = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let default_session = read_json(response).await;
    assert!(
        default_session.get("model_policy").is_none(),
        "omitting model_policy must preserve the existing session representation"
    );

    let response = post(
        &app,
        "/v1/sessions",
        json!({"workspace_id":"local","model_policy":null}),
    )
    .await;
    let null_policy_session = read_json(response).await;
    assert!(null_policy_session.get("model_policy").is_none());

    let response = post(&app, "/v1/sessions", json!({"workspace_id":"local","model_policy":{"provider":" MOCK ","model":" o3 ","reasoning_effort":"high"}})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await;
    assert_eq!(
        session["model_policy"],
        json!({
            "provider": "mock",
            "model": "o3",
            "reasoning_effort": "high"
        })
    );
    let session_id = session["id"].as_str().expect("session id").to_string();

    let response = get(&app, &format!("/v1/sessions/{session_id}")).await;
    let fetched = read_json(response).await;
    assert_eq!(fetched["model_policy"], session["model_policy"]);

    let response = get(&app, "/v1/sessions").await;
    let listed = read_json(response).await;
    let listed_session = listed["data"]
        .as_array()
        .expect("session data")
        .iter()
        .find(|candidate| candidate["id"] == session_id)
        .expect("configured session listed");
    assert_eq!(listed_session["model_policy"], session["model_policy"]);

    let response = get(&app, &format!("/v1/sessions/{session_id}/events")).await;
    let replayed = read_json(response).await;
    assert!(
        replayed["data"]
            .as_array()
            .expect("event data")
            .iter()
            .any(|event| event["event"] == "session.created"
                && event["payload"]["model_policy"] == session["model_policy"]),
        "session history replay must project the normalized policy"
    );

    let response = post(&app, &format!("/v1/sessions/{session_id}/tasks"), json!({"input":{"role":"user","parts":[{"type":"text","text":"exercise the configured route","visibility":"public"}]}})).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task = read_json(response).await;
    let task_id = task["id"].as_str().expect("task id");

    loop {
        let event = events.recv().await.expect("task terminal event");
        if event.task_id.as_deref() != Some(task_id) {
            continue;
        }
        match event.event.as_str() {
            "task.completed" => break,
            "task.failed" => panic!("session model policy did not reach execution"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn updates_forks_clears_and_rejects_invalid_shape() {
    let (_dir, server) = pipeline_server(TEST_PIPELINE);
    let app = api_router(server.state);

    let response = post(
        &app,
        "/v1/sessions",
        json!({"workspace_id":"local","model_policy":{"provider":"mock","model":"o3"}}),
    )
    .await;
    let session = read_json(response).await;
    let session_id = session["id"].as_str().expect("session id");

    let response = request(
        &app,
        "PATCH",
        &format!("/v1/sessions/{session_id}"),
        json!({"model_policy":{"provider":"mock","model":"o3","reasoning_effort":"low"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = read_json(response).await;
    assert_eq!(updated["model_policy"]["reasoning_effort"], "low");

    let response = post(&app, &format!("/v1/sessions/{session_id}/fork"), json!({})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let forked = read_json(response).await;
    assert_eq!(forked["model_policy"], updated["model_policy"]);

    let response = request(
        &app,
        "PATCH",
        &format!("/v1/sessions/{session_id}"),
        json!({"model_policy":null}),
    )
    .await;
    let cleared = read_json(response).await;
    assert!(cleared.get("model_policy").is_none());

    let response = request(
        &app,
        "PATCH",
        &format!("/v1/sessions/{session_id}"),
        json!({"model_policy":{"provider":"mock","model":"o3","preset":"friendly"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = read_json(response).await;
    assert_eq!(error["error"]["code"], "invalid_model_policy");
    assert_eq!(
        error["error"]["message"],
        "model_policy must contain provider, model, and optional reasoning_effort only"
    );

    let response = request(
        &app,
        "PATCH",
        &format!("/v1/sessions/{session_id}"),
        json!({"model_policy":{"provider":"mock","model":"o3","reasoning_effort":"turbo"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = read_json(response).await;
    assert_eq!(error["error"]["code"], "invalid_model_policy");
}
