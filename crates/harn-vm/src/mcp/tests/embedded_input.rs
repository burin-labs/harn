// Requests the client answers itself rather than forwarding to a server:
// `roots/list` and its change notification, and `sampling/createMessage`
// routing into the sampling dispatcher. Split out of `tests.rs` (#6091); the
// cases move verbatim.
use super::*;

#[test]
fn current_mcp_roots_prefers_project_root_over_child_cwd() {
    let root = std::env::temp_dir().join(format!("harn-mcp-roots-{}", uuid::Uuid::now_v7()));
    let child = root.join("nested");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(root.join("harn.toml"), "[package]\nname = \"roots\"\n").unwrap();

    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(child.to_string_lossy().into_owned()),
            source_dir: Some(child.to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    let roots = current_mcp_roots();
    let expected_root = std::fs::canonicalize(&root).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, expected_root.to_string_lossy());
    assert!(roots[0].uri.starts_with("file://"));
    assert_eq!(
        roots[0].name,
        expected_root.file_name().unwrap().to_string_lossy()
    );

    crate::stdlib::process::reset_process_state();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn current_mcp_roots_prefers_explicit_project_root_without_harn_toml() {
    let root = std::env::temp_dir().join(format!("harn-mcp-explicit-{}", uuid::Uuid::now_v7()));
    let child = root.join("nested");
    std::fs::create_dir_all(&child).unwrap();

    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(child.to_string_lossy().into_owned()),
            project_root: Some(root.to_string_lossy().into_owned()),
            source_dir: Some(child.to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    let roots = current_mcp_roots();
    let expected_root = std::fs::canonicalize(&root).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, expected_root.to_string_lossy());

    crate::stdlib::process::reset_process_state();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "current_thread")]
async fn embedded_input_routes_roots_list() {
    let root = std::env::temp_dir().join(format!("harn-mcp-roots-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(root.to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "roots-1",
        "method": crate::mcp_protocol::METHOD_ROOTS_LIST,
    });
    let response = resolve_embedded_input_request("mock", &request, None)
        .await
        .expect("roots/list should produce a response");
    let expected_root = std::fs::canonicalize(&root).unwrap();
    assert_eq!(response["id"], serde_json::json!("roots-1"));
    assert_eq!(response["result"]["roots"].as_array().unwrap().len(), 1);
    assert_eq!(
        response["result"]["roots"][0]["uri"],
        serde_json::json!(url::Url::from_file_path(&expected_root)
            .unwrap()
            .to_string())
    );

    crate::stdlib::process::reset_process_state();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "current_thread")]
async fn roots_list_changed_notification_is_sent_once_per_snapshot() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_recording_http_mcp_server().await;
            let handle = VmMcpClientHandle {
                name: "mock-http".to_string(),
                inner: Arc::new(Mutex::new(Some(McpClientInner::Http(HttpMcpClientInner {
                    client: reqwest::Client::new(),
                    url: format!("{base_url}/mcp"),
                    auth_token: None,
                    auth_token_source: HttpAuthTokenSource::None,
                    token_exchange: None,
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    next_id: 1,
                    proxy_server_name: None,
                    tool_headers: BTreeMap::new(),
                    fixtures: None,
                })))),
                last_roots: Arc::new(Mutex::new(Vec::new())),
                discovery_result: Arc::new(Mutex::new(None)),
                cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
            };

            handle.notify_roots_list_changed_if_needed().await.unwrap();
            let notification = tokio::time::timeout(MCP_TIMEOUT, requests.recv())
                .await
                .expect("timed out waiting for roots notification")
                .expect("mock server closed before notification");
            assert_eq!(
                notification["method"],
                serde_json::json!(crate::mcp_protocol::METHOD_ROOTS_LIST_CHANGED_NOTIFICATION)
            );

            handle.notify_roots_list_changed_if_needed().await.unwrap();
            assert!(requests.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn embedded_input_routes_sampling_to_dispatcher() {
    // Confirms `sampling/createMessage` is routed to
    // `mcp_sampling::dispatch_inbound_sampling` rather than the
    // generic rejection path. With no host bridge installed, the
    // dispatcher declines with the structured `mcp.samplingDeclined`
    // error envelope — proving the request reached the right
    // handler instead of being bounced as `Method not found`.
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": crate::mcp_sampling::SAMPLING_METHOD,
        "params": {
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "ping"}}
            ],
            "maxTokens": 4,
        },
    });
    let response = resolve_embedded_input_request("mock", &request, None)
        .await
        .expect("sampling should produce a response");
    assert_eq!(response["id"], serde_json::json!(42));
    assert_eq!(
        response["error"]["data"]["type"],
        serde_json::json!("mcp.samplingDeclined")
    );
}
