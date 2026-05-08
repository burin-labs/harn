use super::auth::{attach_legacy_deprecation_headers, should_stream_post_response};
use super::*;
use crate::DispatchCoreConfig;

#[tokio::test]
async fn tools_list_exposes_public_functions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let tools = server.tools_list_result(&json!({}));
    assert_eq!(tools["tools"][0]["name"], "greet");
    assert_eq!(tools["tools"][0]["annotations"]["readOnlyHint"], false);
    assert_eq!(tools["tools"][0]["annotations"]["destructiveHint"], true);
    assert_eq!(tools["tools"][0]["inputSchema"]["type"], "object");
    assert_eq!(tools["tools"][0]["outputSchema"]["type"], "string");
}

#[tokio::test]
async fn initialize_and_resources_expose_server_card() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(
        McpServerConfig::new(core)
            .with_server_card(json!({"name": "fixture-card", "version": "1"})),
    );

    let session = SharedSession::new();
    let init = server.handle_initialize(
        json!(1),
        &session,
        &json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {"name": "test", "version": "1"}
        }),
    );
    assert_eq!(
        init["result"]["serverInfo"]["card"]["name"],
        json!("fixture-card")
    );
    assert!(init["result"]["capabilities"]["resources"].is_object());

    let resources = server.resources_list_result(&json!({}));
    assert_eq!(
        resources["resources"][0]["uri"],
        json!("well-known://mcp-card")
    );
    let read = server.handle_resources_read(json!(2), &json!({"uri": "well-known://mcp-card"}));
    assert!(read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("fixture-card"));
}

#[tokio::test]
async fn adapter_protocol_fixture_matches_checked_in_matrix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(
        McpServerConfig::new(core)
            .with_server_card(json!({"name": "fixture-card", "version": "1"})),
    );
    let session = SharedSession::new();

    let initialize = harn_vm::jsonrpc::request(
        1,
        "initialize",
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "fixture-client", "version": "1"}
        }),
    );
    let tools_list = harn_vm::jsonrpc::request(2, "tools/list", json!({}));
    let resources_list = harn_vm::jsonrpc::request(3, "resources/list", json!({}));
    let resources_read =
        harn_vm::jsonrpc::request(4, "resources/read", json!({"uri": "well-known://mcp-card"}));

    let actual = vec![
        initialize.clone(),
        mcp_response(&server, initialize, session.clone()).await,
        harn_vm::jsonrpc::notification("notifications/initialized", json!({})),
        tools_list.clone(),
        mcp_response(&server, tools_list, session.clone()).await,
        resources_list.clone(),
        mcp_response(&server, resources_list, session.clone()).await,
        resources_read.clone(),
        mcp_response(&server, resources_read, session).await,
    ];
    crate::protocol_fixture_tests::assert_fixture_documents_match(
        "conformance/protocols/fixtures/mcp/adapter_initialize_tools_resources.valid.json",
        actual,
    );
}

async fn mcp_response(server: &McpServer, request: JsonValue, session: SharedSession) -> JsonValue {
    match server
        .process_message(request, session, AuthRequest::default())
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted | ImmediateResult::Stream(_) => {
            panic!("expected MCP JSON-RPC response")
        }
    }
}

#[tokio::test]
async fn latest_spec_gap_methods_return_explicit_json_rpc_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let session = SharedSession::new();
    let _ = server.handle_initialize(
        json!(1),
        &session,
        &json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {"name": "test", "version": "1"}
        }),
    );

    for method in harn_vm::mcp_protocol::UNSUPPORTED_LATEST_SPEC_METHODS
        .iter()
        .map(|entry| entry.method)
    {
        let response = match server
            .process_message(
                harn_vm::jsonrpc::request(2, method, json!({})),
                session.clone(),
                AuthRequest::default(),
            )
            .await
        {
            ImmediateResult::Response(response) => response,
            ImmediateResult::Accepted | ImmediateResult::Stream(_) => {
                panic!("expected error response for {method}")
            }
        };
        assert_eq!(response["error"]["code"], json!(-32601), "{method}");
        assert_eq!(response["error"]["data"]["method"], json!(method));
        assert_eq!(response["error"]["data"]["status"], json!("unsupported"));
    }
}

#[tokio::test]
async fn tool_call_rejects_task_augmentation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let session = SharedSession::new();
    let _ = server.handle_initialize(
        json!(1),
        &session,
        &json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {"name": "test", "version": "1"}
        }),
    );

    let response = match server
        .process_message(
            harn_vm::jsonrpc::request(
                2,
                "tools/call",
                json!({
                    "name": "greet",
                    "arguments": {"name": "alice"},
                    "task": {"title": "async please"}
                }),
            ),
            session,
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted | ImmediateResult::Stream(_) => {
            panic!("expected task-augmentation error response")
        }
    };

    assert_eq!(response["error"]["code"], json!(-32602));
    assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
}

#[test]
fn paged_result_returns_next_cursor_and_decodes_it() {
    let entries = (0..55)
        .map(|index| json!({"name": format!("tool-{index}")}))
        .collect::<Vec<_>>();
    let first = paged_result("tools", entries.clone(), &json!({}));
    assert_eq!(first["tools"].as_array().unwrap().len(), 50);
    assert_eq!(first["tools"][49]["name"], json!("tool-49"));

    let second = paged_result(
        "tools",
        entries,
        &json!({"cursor": first["nextCursor"].as_str().unwrap()}),
    );
    assert_eq!(second["tools"].as_array().unwrap().len(), 5);
    assert_eq!(second["tools"][0]["name"], json!("tool-50"));
    assert!(second.get("nextCursor").is_none());
}

#[test]
fn build_call_request_accepts_named_arguments() {
    let request = build_call_request(
        "mcp",
        "tester",
        "greet",
        json!({"name": "alice"}),
        AuthRequest::default(),
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .expect("call request");
    match request.arguments {
        CallArguments::Named(values) => assert_eq!(values["name"], json!("alice")),
        other => panic!("expected named arguments, got {other:?}"),
    }
}

#[test]
fn streamable_http_accept_negotiation_uses_sse_only_when_json_is_absent() {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    assert!(should_stream_post_response(&headers));

    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    assert!(!should_stream_post_response(&headers));
}

#[test]
fn legacy_deprecation_header_is_attached() {
    let mut response = StatusCode::ACCEPTED.into_response();
    attach_legacy_deprecation_headers(&mut response);
    assert_eq!(
        response
            .headers()
            .get(DEPRECATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
}
