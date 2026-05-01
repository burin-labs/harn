use super::support::*;

#[tokio::test(flavor = "multi_thread")]
async fn github_webhook_delivery_is_accepted_and_persisted() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "integration-test-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let health = reqwest::get(format!("{base_url}/health")).await.unwrap();
    assert_status(health, StatusCode::OK).await;

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    await_pump_dispatch_completed(&harness).await;
    shutdown(harness).await;

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"status\": \"stopped\""),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"received\": 1"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn github_provider_prefers_configured_harn_connector_over_deprecated_rust_default() {
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("github-override-handler.txt");
    write_file(
        temp.path(),
        "harn.toml",
        &github_harn_override_manifest(None),
    );
    write_file(
        temp.path(),
        "lib.harn",
        &github_marker_handler_module(&marker_path),
    );
    write_file(
        temp.path(),
        "github_connector.harn",
        github_override_connector_module(),
    );

    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", "override-secret"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt-gh-override-1",
        "action": "opened",
        "issue": {"number": 42, "title": "Harn connector override"}
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .header(CONTENT_TYPE, "application/json")
        .header("X-GitHub-Event", "issues")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    await_pump_dispatch_completed(&harness).await;
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "issues");

    let event_log = harness.event_log();
    shutdown(harness).await;

    let lifecycle = read_topic_events(&event_log, "connectors.github.override").await;
    let lifecycle_kinds: Vec<_> = lifecycle
        .iter()
        .map(|(_, event)| event.kind.as_str())
        .collect();
    assert_eq!(lifecycle_kinds, vec!["init", "normalize"]);
}
