use super::support::*;
use crate::test_util::process::harn_e2e_command;
use harn_cli::commands::orchestrator::tls::TlsFiles;

#[tokio::test(flavor = "multi_thread")]
async fn admin_reload_endpoint_applies_manifest_changes() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    let original = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers.clone())
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-before"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(original, StatusCode::OK).await;

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-v2"),
    );

    let reload = client
        .post(format!("{base_url}/admin/reload"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({"source": "http_test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::OK);
    let reload_body: JsonValue = reload.json().await.unwrap();
    assert_eq!(reload_body["status"], serde_json::json!("ok"));
    assert_eq!(reload_body["source"], serde_json::json!("http_test"));
    assert_eq!(
        reload_body["summary"]["modified"][0],
        serde_json::json!("incoming-review-task")
    );

    let updated = client
        .post(format!("{base_url}/a2a/review-v2"))
        .headers(auth_headers.clone())
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-after"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(updated, StatusCode::OK).await;

    let retired = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-old"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(retired, StatusCode::NOT_FOUND).await;

    shutdown(harness).await;
    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"listener_url\""), "snapshot={snapshot}");
    assert!(snapshot.contains("\"version\": 2"), "snapshot={snapshot}");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_reload_invalid_manifest_keeps_existing_routes_live() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    write_file(
        temp.path(),
        "harn.toml",
        "[package]\nname = \"broken\"\n[[triggers]]\nid = ",
    );

    let reload = client
        .post(format!("{base_url}/admin/reload"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({"source": "http_test_invalid"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = reload.text().await.unwrap();
    assert!(body.contains("error"), "{body}");

    let still_live = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-still-live"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(still_live, StatusCode::OK).await;

    shutdown(harness).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_mode_reload_handle_applies_manifest_changes() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ])
    .await;
    let harness = start_harness_with(&temp, |mut config| {
        config.watch_manifest = true;
        config
    })
    .await;
    let base_url = harness.listener_url().to_string();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-watch"),
    );
    let reload = client
        .post(format!("{base_url}/admin/reload"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({"source": "file_watch_test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::OK);
    let reload_body: JsonValue = reload.json().await.unwrap();
    assert_eq!(reload_body["status"], serde_json::json!("ok"));
    assert_eq!(reload_body["source"], serde_json::json!("file_watch_test"));
    let summary = &reload_body["summary"];
    assert_eq!(
        summary["modified"],
        serde_json::json!(["incoming-review-task"])
    );

    let manifest_events = read_topic_events(&harness.event_log(), "orchestrator.manifest").await;
    assert!(
        manifest_events.iter().any(|(_, event)| {
            event.kind == "reload_succeeded"
                && event.payload["source"] == serde_json::json!("file_watch_test")
                && event.payload["summary"]["modified"]
                    == serde_json::json!(["incoming-review-task"])
        }),
        "manifest_events={manifest_events:?}"
    );

    let response = client
        .post(format!("{base_url}/a2a/review-watch"))
        .headers(auth_headers.clone())
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-watch"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let retired = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-old"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(retired, StatusCode::NOT_FOUND).await;

    shutdown(harness).await;
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[tokio::test(flavor = "multi_thread")]
async fn reload_cli_uses_admin_endpoint() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let _envs = lock_env_with(&envs).await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-cli"),
    );

    let output = harn_e2e_command()
        .current_dir(temp.path())
        .arg("orchestrator")
        .arg("reload")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state")
        .envs(envs)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("reload ok"),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));
    let updated = client
        .post(format!("{base_url}/a2a/review-cli"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-cli"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(updated, StatusCode::OK).await;

    shutdown(harness).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_listener_serves_https_with_supplied_cert_and_key() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());
    write_file(
        temp.path(),
        "github_connector.harn",
        github_connector_module(),
    );

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .unwrap();
    let cert_pem = cert.cert.pem();
    let key_pem = cert.signing_key.serialize_pem();
    write_bytes(temp.path(), "tls/cert.pem", cert_pem.as_bytes());
    write_bytes(temp.path(), "tls/key.pem", key_pem.as_bytes());

    let secret = "tls-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ])
    .await;
    let cert_path = temp.path().join("tls/cert.pem");
    let key_path = temp.path().join("tls/key.pem");
    let harness = start_harness_with(&temp, move |mut config| {
        config.tls = Some(TlsFiles::new(cert_path, key_path));
        config
    })
    .await;
    let base_url = harness.listener_url().to_string();
    assert!(base_url.starts_with("https://"), "{base_url}");

    let body = br#"{"action":"opened","issue":{"number":2}}"#;
    let response = reqwest::Client::builder()
        .add_root_certificate(Certificate::from_pem(cert_pem.as_bytes()).unwrap())
        .build()
        .unwrap()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    shutdown(harness).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disallowed_origin_is_rejected() {
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        &base_manifest(Some(
            r#"[orchestrator]
allowed_origins = ["https://allowed.example"]"#,
        )),
    );
    write_file(temp.path(), "lib.harn", handler_module());
    write_file(
        temp.path(),
        "github_connector.harn",
        github_connector_module(),
    );

    let secret = "origin-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let body = br#"{"action":"opened","issue":{"number":3}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(
            secret,
            body,
            Some("https://blocked.example"),
        ))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::FORBIDDEN).await;

    shutdown(harness).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_request_body_is_rejected() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());
    write_file(
        temp.path(),
        "github_connector.harn",
        github_connector_module(),
    );

    let secret = "body-limit-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let body = vec![b'a'; (10 * 1024 * 1024) + 1];
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, &body, None))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::PAYLOAD_TOO_LARGE).await;

    shutdown(harness).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_waits_for_in_flight_request() {
    let temp = TempDir::new().unwrap();
    let request_entered_path = temp.path().join("request-entered");
    let request_release_path = temp.path().join("request-release");
    let request_entered_value = request_entered_path.to_string_lossy().into_owned();
    let request_release_value = request_release_path.to_string_lossy().into_owned();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());
    write_file(
        temp.path(),
        "github_connector.harn",
        github_connector_module(),
    );

    let secret = "shutdown-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        (
            "HARN_ORCHESTRATOR_TEST_REQUEST_ENTERED_FILE",
            request_entered_value.as_str(),
        ),
        (
            "HARN_ORCHESTRATOR_TEST_REQUEST_RELEASE_FILE",
            request_release_value.as_str(),
        ),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();
    let shutdown_trigger = harness.shutdown_trigger();

    let body = br#"{"action":"opened","issue":{"number":4}}"#.to_vec();
    let request = tokio::spawn({
        let client = reqwest::Client::new();
        let url = format!("{base_url}/triggers/github-new-issue");
        let headers = github_headers(secret, &body, None);
        async move { client.post(url).headers(headers).body(body).send().await }
    });

    wait_for_path(&request_entered_path).await;

    let _ = shutdown_trigger.send(true);
    fs::write(&request_release_path, b"release").unwrap();
    let response = request.await.unwrap().unwrap();
    assert_status(response, StatusCode::OK).await;

    shutdown(harness).await;

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"in_flight\": 0"), "snapshot={snapshot}");
}
