use super::*;

#[tokio::test]
async fn round_trips_and_reaches_execution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    std::fs::write(
        &script,
        r#"pipeline main() {
  llm_mock_clear()
  llm_mock({text: "ok", model: "o3", provider: "mock"})
  llm_call(prompt)
  const call = llm_mock_calls()[0]
  assert(
    call.thinking.mode == "effort",
    "session reasoning mode reached provider: ${json_stringify(call.thinking)}",
  )
  assert(call.thinking.level == "high", "session reasoning effort reached provider")
  __io_println("session-model-policy-fired")
}
"#,
    )
    .expect("write script");
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let state = server.state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace_id":"local"}"#))
                .expect("request"),
        )
        .await
        .expect("default session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let default_session: Value = serde_json::from_slice(&body).expect("session");
    assert!(
        default_session.get("model_policy").is_none(),
        "omitting model_policy must preserve the existing session representation"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"workspace_id":"local","model_policy":null}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("null policy session response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let null_policy_session: Value = serde_json::from_slice(&body).expect("session");
    assert!(null_policy_session.get("model_policy").is_none());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"workspace_id":"local","model_policy":{"provider":" MOCK ","model":" o3 ","reasoning_effort":"high"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let session: Value = serde_json::from_slice(&body).expect("session");
    assert_eq!(
        session["model_policy"],
        json!({
            "provider": "mock",
            "model": "o3",
            "reasoning_effort": "high"
        })
    );
    let session_id = session["id"].as_str().expect("session id").to_string();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sessions/{session_id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("get session response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let fetched: Value = serde_json::from_slice(&body).expect("session");
    assert_eq!(fetched["model_policy"], session["model_policy"]);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/sessions")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("list sessions response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let listed: Value = serde_json::from_slice(&body).expect("session list");
    let listed_session = listed["data"]
        .as_array()
        .expect("session data")
        .iter()
        .find(|candidate| candidate["id"] == session_id)
        .expect("configured session listed");
    assert_eq!(listed_session["model_policy"], session["model_policy"]);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sessions/{session_id}/events"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("replayed events response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let replayed: Value = serde_json::from_slice(&body).expect("event list");
    assert!(
        replayed["data"]
            .as_array()
            .expect("event data")
            .iter()
            .any(|event| event["event"] == "session.created"
                && event["payload"]["model_policy"] == session["model_policy"]),
        "session history replay must project the normalized policy"
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/tasks"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"input":{"role":"user","parts":[{"type":"text","text":"exercise the configured route","visibility":"public"}]}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("task response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let task: Value = serde_json::from_slice(&body).expect("task");
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
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    std::fs::write(&script, "pipeline main() { __io_println(prompt) }\n").expect("write script");
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let app = api_router(server.state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"workspace_id":"local","model_policy":{"provider":"mock","model":"o3"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("session response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let session: Value = serde_json::from_slice(&body).expect("session");
    let session_id = session["id"].as_str().expect("session id");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/sessions/{session_id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model_policy":{"provider":"mock","model":"o3","reasoning_effort":"low"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("update response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let updated: Value = serde_json::from_slice(&body).expect("session");
    assert_eq!(updated["model_policy"]["reasoning_effort"], "low");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/fork"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("fork response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let forked: Value = serde_json::from_slice(&body).expect("forked session");
    assert_eq!(forked["model_policy"], updated["model_policy"]);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/sessions/{session_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model_policy":null}"#))
                .expect("request"),
        )
        .await
        .expect("clear response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let cleared: Value = serde_json::from_slice(&body).expect("session");
    assert!(cleared.get("model_policy").is_none());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/sessions/{session_id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model_policy":{"provider":"mock","model":"o3","preset":"friendly"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("invalid update response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let error: Value = serde_json::from_slice(&body).expect("error");
    assert_eq!(error["error"]["code"], "invalid_model_policy");
    assert_eq!(
        error["error"]["message"],
        "model_policy must contain provider, model, and optional reasoning_effort only"
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/sessions/{session_id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model_policy":{"provider":"mock","model":"o3","reasoning_effort":"turbo"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("invalid effort response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let error: Value = serde_json::from_slice(&body).expect("error");
    assert_eq!(error["error"]["code"], "invalid_model_policy");
}
