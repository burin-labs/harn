use super::support::*;

#[tokio::test(flavor = "multi_thread")]
async fn slack_webhook_acknowledges_before_handler_finishes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("slack-handler.txt");
    let release_path = temp.path().join("release-slack-dispatch");
    let release_path_value = release_path.to_string_lossy().into_owned();
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(temp.path(), "lib.harn", &slack_handler_module(&marker_path));

    let secret = "slack-signing-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_SLACK_SIGNING_SECRET", secret),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            release_path_value.as_str(),
        ),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let body = serde_json::to_vec(&serde_json::json!({
        "token": "ZZZZZZWSxiZZZ2yIvs3peJ",
        "team_id": "T123ABC456",
        "api_app_id": "A123ABC456",
        "event": {
            "type": "app_mention",
            "user": "U123ABC456",
            "text": "What is the hour of the pearl, <@U0LAN0Z89>?",
            "ts": "1515449522.000016",
            "channel": "C123ABC456",
            "event_ts": "1515449522000016"
        },
        "type": "event_callback",
        "event_id": "Ev123ABC456",
        "event_time": 1515449522000016i64
    }))
    .unwrap();

    let response = tokio::time::timeout(
        timing::SLACK_ACK_TIMEOUT,
        reqwest::Client::new()
            .post(format!("{base_url}/triggers/slack-mentions"))
            .headers(slack_headers(secret, timestamp, &body))
            .body(body)
            .send(),
    )
    .await
    .unwrap_or_else(|_| panic!("slack ack path exceeded {:?}", timing::SLACK_ACK_TIMEOUT))
    .unwrap();
    assert_status(response, StatusCode::OK).await;
    assert!(
        !marker_path.exists(),
        "dispatch should not have completed before the HTTP ack"
    );
    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_admitted" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;
    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_acked" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;
    assert!(
        !marker_path.exists(),
        "dispatch should still be blocked on the explicit release gate"
    );
    fs::write(&release_path, b"release").unwrap();
    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_dispatch_completed"
            && event.payload["event_log_id"] == serde_json::json!(1)
            && event.payload["status"] == serde_json::json!("completed")
    })
    .await;
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "app_mention");

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn slack_url_verification_returns_plaintext_challenge() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &slack_handler_module(&temp.path().join("unused-slack-marker.txt")),
    );

    let secret = "slack-signing-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_SLACK_SIGNING_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let body = serde_json::to_vec(&serde_json::json!({
        "token": "legacy-token",
        "challenge": "3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P",
        "type": "url_verification"
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(slack_headers(secret, timestamp, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response.text().await.unwrap();
    assert_eq!(
        response_body,
        "3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P"
    );

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn slack_bad_requests_set_no_retry_header_and_export_delivery_metrics() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &slack_handler_module(&temp.path().join("unused-slack-marker.txt")),
    );

    let secret = "slack-signing-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_SLACK_SIGNING_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let body = serde_json::to_vec(&serde_json::json!({
        "token": "ZZZZZZWSxiZZZ2yIvs3peJ",
        "team_id": "T123ABC456",
        "api_app_id": "A123ABC456",
        "event": {
            "type": "app_mention",
            "user": "U123ABC456",
            "text": "hello",
            "ts": "1515449522.000016",
            "channel": "C123ABC456",
            "event_ts": "1515449522000016"
        },
        "type": "event_callback",
        "event_id": "Ev123ABC456",
        "event_time": 1515449522
    }))
    .unwrap();

    let mut bad_headers = slack_headers(secret, timestamp, &body);
    bad_headers.insert(
        "X-Slack-Signature",
        HeaderValue::from_static(
            "v0=0000000000000000000000000000000000000000000000000000000000000000",
        ),
    );
    let bad = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(bad_headers)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        bad.headers()
            .get("x-slack-no-retry")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );

    let ok = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(slack_headers(secret, timestamp, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    let metrics = reqwest::Client::new()
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("slack_events_delivery_success_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("slack_events_delivery_failure_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("slack_events_auto_disable_min_success_ratio 0.05"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("harn_http_requests_total{endpoint=\"/triggers/slack-mentions\",method=\"POST\",status=\"200\"} 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains(
            "harn_trigger_received_total{provider=\"slack\",trigger_id=\"slack-mentions\"} 2"
        ),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains(
            "harn_event_log_append_duration_seconds_bucket{le=\"+Inf\",topic=\"orchestrator.triggers.pending\""
        ),
        "metrics={metrics}"
    );

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}
