use super::support::*;

#[tokio::test(flavor = "multi_thread")]
async fn admin_reload_endpoint_applies_manifest_changes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

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

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"listener_url\""), "snapshot={snapshot}");
    assert!(snapshot.contains("\"version\": 2"), "snapshot={snapshot}");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_reload_invalid_manifest_keeps_existing_routes_live() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

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

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_mode_reloads_manifest_changes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &["--watch"], &envs);
    let base_url = process.wait_for_listener_url();

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-watch"),
    );

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    loop {
        let response = client
            .post(format!("{base_url}/a2a/review-watch"))
            .headers(auth_headers.clone())
            .body(br#"{"kind":"a2a.task.received","task":{"id":"task-watch"}}"#.to_vec())
            .send()
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            break;
        }
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(Instant::now() < deadline, "watch reload never applied");
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    }

    let retired = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-old"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(retired, StatusCode::NOT_FOUND).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_cli_uses_admin_endpoint() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-cli"),
    );

    let output = harn_command()
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

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_listener_serves_https_with_supplied_cert_and_key() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .unwrap();
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();
    write_bytes(temp.path(), "tls/cert.pem", cert_pem.as_bytes());
    write_bytes(temp.path(), "tls/key.pem", key_pem.as_bytes());

    let secret = "tls-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ];
    let args = ["--cert", "tls/cert.pem", "--key", "tls/key.pem"];
    let mut process = spawn_orchestrator(&temp, &args, &envs);
    let base_url = process.wait_for_listener_url();
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

    send_sigterm(&mut process.child);
    let status = process
        .child
        .wait_timeout(PROCESS_FAIL_FAST_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"));
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn disallowed_origin_is_rejected() {
    let _lock = lock_orchestrator_tests();
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

    let secret = "origin-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

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

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_request_body_is_rejected() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "body-limit-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = vec![b'a'; (10 * 1024 * 1024) + 1];
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, &body, None))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::PAYLOAD_TOO_LARGE).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_waits_for_in_flight_request() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let request_entered_path = temp.path().join("request-entered");
    let request_release_path = temp.path().join("request-release");
    let request_entered_value = request_entered_path.to_string_lossy().into_owned();
    let request_release_value = request_release_path.to_string_lossy().into_owned();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "shutdown-secret";
    let envs = [
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
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":4}}"#.to_vec();
    let request = tokio::spawn({
        let client = reqwest::Client::new();
        let url = format!("{base_url}/triggers/github-new-issue");
        let headers = github_headers(secret, &body, None);
        async move { client.post(url).headers(headers).body(body).send().await }
    });

    wait_for_path(&request_entered_path, EVENT_FAIL_FAST_TIMEOUT);
    send_sigterm(&mut process.child);
    fs::write(&request_release_path, b"release").unwrap();
    let response = request.await.unwrap().unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"in_flight\": 0"), "snapshot={snapshot}");
}
