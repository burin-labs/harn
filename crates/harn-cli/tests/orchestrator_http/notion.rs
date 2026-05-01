use super::support::*;

#[tokio::test(flavor = "multi_thread")]
async fn notion_webhook_handshake_is_captured_and_reported_by_doctor() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &notion_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &notion_handler_module(&temp.path().join("unused-notion-marker.txt")),
    );

    let envs = [("HARN_SECRET_PROVIDERS", "env")];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "verification_token": "secret_notion_test_token"
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/hooks/notion"))
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload: JsonValue = response.json().await.unwrap();
    assert_eq!(
        payload.get("status").and_then(JsonValue::as_str),
        Some("handshake_captured")
    );
    assert_eq!(
        payload
            .get("verification_token")
            .and_then(JsonValue::as_str),
        Some("secret_notion_test_token")
    );

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");

    let doctor = harn_command()
        .current_dir(temp.path())
        .arg("doctor")
        .arg("--no-network")
        .env("HARN_SECRET_PROVIDERS", "env")
        .env("HARN_EVENT_LOG_SQLITE_PATH", "state/events.sqlite")
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "doctor failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        stdout.contains("WARN  notion:notion-pages"),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains("captured verification_token=secret_notion_test_token"),
        "stdout={stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notion_webhook_signed_delivery_is_dispatched() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("notion-handler.txt");
    write_file(temp.path(), "harn.toml", &notion_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &notion_handler_module(&marker_path),
    );

    let secret = "secret-notion-live-token";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_NOTION_VERIFICATION_TOKEN", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt_notion_1",
        "timestamp": "2026-04-19T12:34:56Z",
        "type": "page.content_updated",
        "workspace_id": "ws_123",
        "subscription_id": "sub_123",
        "integration_id": "int_123",
        "attempt_number": 1,
        "entity": {
            "id": "page_123",
            "type": "page"
        }
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/hooks/notion"))
        .headers(notion_headers(secret, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_path(&marker_path, EVENT_FAIL_FAST_TIMEOUT);
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "page.content_updated");

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"received\": 1"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}
