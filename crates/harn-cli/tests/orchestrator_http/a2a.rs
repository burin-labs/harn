use super::support::*;

#[tokio::test(flavor = "multi_thread")]
async fn harn_connector_module_round_trips_inbound_and_client_calls() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("echo-handler.json");
    write_file(temp.path(), "harn.toml", &echo_manifest(None));
    write_file(temp.path(), "lib.harn", &echo_handler_module(&marker_path));
    write_file(temp.path(), "echo_connector.harn", echo_connector_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_ECHO_API_TOKEN", "echo-secret-token"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt_echo_1",
        "message": "hello from echo"
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/hooks/echo"))
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    let marker = wait_for_json_file(&marker_path, EVENT_FAIL_FAST_TIMEOUT);
    assert_eq!(
        marker.get("kind").and_then(JsonValue::as_str),
        Some("echo.received")
    );
    assert_eq!(
        marker.get("token").and_then(JsonValue::as_str),
        Some("echo-secret-token")
    );
    assert_eq!(
        marker.get("binding_id").and_then(JsonValue::as_str),
        Some("echo-webhook")
    );
    assert_eq!(
        marker.get("echoed").and_then(JsonValue::as_str),
        Some("hello from echo")
    );
    assert_eq!(
        marker.get("ping_token").and_then(JsonValue::as_str),
        Some("echo-secret-token")
    );

    let metrics = reqwest::Client::new()
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("connector_custom_echo_activate_bindings_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("connector_custom_echo_normalize_calls_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("connector_custom_echo_client_calls_total 1"),
        "metrics={metrics}"
    );

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let lifecycle = read_topic_events(&temp, "connectors.echo.lifecycle").await;
    let lifecycle_kinds: Vec<_> = lifecycle
        .iter()
        .map(|(_, event)| event.kind.as_str())
        .collect();
    assert_eq!(
        lifecycle_kinds,
        vec!["init", "activate", "normalize", "shutdown"]
    );
    let normalize_event = lifecycle
        .iter()
        .find(|(_, event)| event.kind == "normalize")
        .expect("normalize event");
    assert_eq!(
        normalize_event
            .1
            .payload
            .get("binding_id")
            .and_then(JsonValue::as_str),
        Some("echo-webhook")
    );
    assert_eq!(
        normalize_event
            .1
            .payload
            .get("message")
            .and_then(JsonValue::as_str),
        Some("hello from echo")
    );

    let calls = read_topic_events(&temp, "connectors.echo.calls").await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.kind, "ping");
    assert_eq!(
        calls[0]
            .1
            .payload
            .get("message")
            .and_then(JsonValue::as_str),
        Some("hello from echo")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_trigger_route_uses_generic_stream_connector() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("stream-handler.json");
    write_file(temp.path(), "harn.toml", &stream_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &stream_handler_module(&marker_path),
    );

    let mut process = spawn_orchestrator(&temp, &[], &[("HARN_SECRET_PROVIDERS", "env")]);
    let base_url = process.wait_for_listener_url();

    let response = reqwest::Client::new()
        .post(format!("{base_url}/streams/ws"))
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({
            "key": "acct-1",
            "stream": "quotes",
            "value": {"amount": 10}
        }))
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_dispatch_completed"
            && event.payload["status"] == serde_json::json!("completed")
    })
    .await;
    let marker: JsonValue =
        serde_json::from_str(&fs::read_to_string(&marker_path).unwrap()).unwrap();
    assert_eq!(
        marker.get("provider").and_then(JsonValue::as_str),
        Some("websocket")
    );
    assert_eq!(
        marker.get("kind").and_then(JsonValue::as_str),
        Some("quote.tick")
    );
    assert_eq!(
        marker.get("key").and_then(JsonValue::as_str),
        Some("acct-1")
    );
    assert_eq!(
        marker.get("stream").and_then(JsonValue::as_str),
        Some("quotes")
    );
    assert_eq!(marker.get("amount").and_then(JsonValue::as_i64), Some(10));

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(
        stderr.contains("activated connectors: websocket(1)"),
        "stderr={stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a2a_push_route_requires_bearer_or_valid_hmac() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key-1,test-key-2"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-123"}}"#;

    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(json_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::UNAUTHORIZED).await;

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let mut wrong_hmac_headers = json_headers();
    wrong_hmac_headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!(
            "HMAC-SHA256 timestamp={timestamp},signature=AAAAAAAAAAAAAAAAAAAAAA=="
        ))
        .unwrap(),
    );
    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(wrong_hmac_headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::UNAUTHORIZED).await;

    let mut bearer_headers = json_headers();
    bearer_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key-2"));
    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"received\": 3"), "snapshot={snapshot}");
    assert!(snapshot.contains("\"failed\": 2"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn embedded_mcp_endpoint_serves_orchestrator_tools_on_listener() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "mcp-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &["--mcp"], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer mcp-key"));
    let initialize = client
        .post(format!("{base_url}/mcp"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "clientInfo": { "name": "orchestrator-test", "version": "0" },
                "capabilities": { "harn": { "apiKey": "mcp-key" } }
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(initialize.status(), StatusCode::OK);
    let session_id = initialize
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("MCP session header")
        .to_string();
    let initialize_body: JsonValue = initialize.json().await.unwrap();
    assert_eq!(
        initialize_body["result"]["serverInfo"]["name"],
        serde_json::json!("harn-orchestrator")
    );

    auth_headers.insert(
        "mcp-session-id",
        HeaderValue::from_str(&session_id).unwrap(),
    );
    let tools = client
        .post(format!("{base_url}/mcp"))
        .headers(auth_headers)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(tools.status(), StatusCode::OK);
    let tools_body: JsonValue = tools.json().await.unwrap();
    assert!(
        tools_body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "harn.orchestrator.inspect"),
        "tools={tools_body}"
    );

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(
        stderr.contains("embedded MCP server mounted at /mcp"),
        "stderr={stderr}"
    );
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}
