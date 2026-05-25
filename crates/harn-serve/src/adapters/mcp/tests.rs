use super::auth::{attach_legacy_deprecation_headers, should_stream_post_response};
use super::*;
use crate::{ApiKeyAuthConfig, AuthMethodConfig, DispatchCoreConfig};
use std::collections::{BTreeMap, BTreeSet};

#[tokio::test]
async fn tools_list_exposes_public_functions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
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
        r"
pub fn greet(name: string) -> string {
  return name
}
",
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
async fn package_context_resources_templates_prompts_and_completions_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        dir.path().join("harn.toml"),
        "[package]\nname = \"fixture\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.path().join("README.md"), "# Fixture\n").expect("write readme");
    std::fs::create_dir_all(dir.path().join("prompts")).expect("prompt dir");
    std::fs::write(
        dir.path().join("prompts/review.harn.prompt"),
        r#"---
id = "review"
description = "Review code"
[[arguments]]
name = "language"
required = false
suggestions = ["rust", "ruby", "typescript"]
[[arguments]]
name = "code"
required = true
---
Review {{ language }}: {{ code }}
"#,
    )
    .expect("write prompt");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let session = SharedSession::new();
    let init = server.handle_initialize(
        json!(1),
        &session,
        &json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {"name": "test", "version": "1"}
        }),
    );
    assert!(init["result"]["capabilities"]["resources"].is_object());
    assert!(init["result"]["capabilities"]["prompts"].is_object());
    assert!(init["result"]["capabilities"]["completions"].is_object());

    let resources = mcp_response(
        &server,
        harn_vm::jsonrpc::request(2, "resources/list", json!({})),
        session.clone(),
    )
    .await;
    let uris = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(uris.contains(&"harn://package/manifest"));
    assert!(uris.contains(&"harn://package/readme"));
    assert!(uris.contains(&"harn://package/source"));
    assert!(uris.contains(&"harn://prompt/review/source"));

    let source = mcp_response(
        &server,
        harn_vm::jsonrpc::request(3, "resources/read", json!({"uri": "harn://package/source"})),
        session.clone(),
    )
    .await;
    assert!(source["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("pub fn greet"));

    let templates = mcp_response(
        &server,
        harn_vm::jsonrpc::request(4, "resources/templates/list", json!({})),
        session.clone(),
    )
    .await;
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        json!("harn://package/{artifact}")
    );
    assert_eq!(
        templates["result"]["resourceTemplates"][1]["uriTemplate"],
        json!("harn://prompt/{name}/source")
    );

    let prompts = mcp_response(
        &server,
        harn_vm::jsonrpc::request(5, "prompts/list", json!({})),
        session.clone(),
    )
    .await;
    assert_eq!(prompts["result"]["prompts"][0]["name"], json!("review"));

    let prompt = mcp_response(
        &server,
        harn_vm::jsonrpc::request(
            6,
            "prompts/get",
            json!({"name": "review", "arguments": {"language": "Rust", "code": "fn main() {}"}}),
        ),
        session.clone(),
    )
    .await;
    assert!(prompt["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("fn main"));

    let prompt_completion = mcp_response(
        &server,
        harn_vm::jsonrpc::request(
            7,
            mcp_protocol::METHOD_COMPLETION_COMPLETE,
            json!({
                "ref": {"type": "ref/prompt", "name": "review"},
                "argument": {"name": "language", "value": "ru"}
            }),
        ),
        session.clone(),
    )
    .await;
    assert_eq!(
        prompt_completion["result"]["completion"]["values"],
        json!(["ruby", "rust"])
    );

    let resource_completion = mcp_response(
        &server,
        harn_vm::jsonrpc::request(
            8,
            mcp_protocol::METHOD_COMPLETION_COMPLETE,
            json!({
                "ref": {"type": "ref/resource", "uri": "harn://package/{artifact}"},
                "argument": {"name": "artifact", "value": "ma"}
            }),
        ),
        session,
    )
    .await;
    assert_eq!(
        resource_completion["result"]["completion"]["values"],
        json!(["manifest"])
    );
}

#[tokio::test]
async fn protocol_context_requires_configured_auth() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
    )
    .expect("write script");
    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = crate::AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: BTreeSet::from(["secret".to_string()]),
        })],
    };
    let core = DispatchCore::new(config).expect("core");
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

    let unauthorized = mcp_response(
        &server,
        harn_vm::jsonrpc::request(2, "resources/list", json!({})),
        session.clone(),
    )
    .await;
    assert_eq!(unauthorized["error"]["code"], json!(-32001));

    let authorized = match server
        .process_message(
            harn_vm::jsonrpc::request(3, "resources/list", json!({})),
            session,
            AuthRequest {
                headers: BTreeMap::from([(
                    "authorization".to_string(),
                    "Bearer secret".to_string(),
                )]),
                ..AuthRequest::default()
            },
        )
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted | ImmediateResult::Stream(_) => {
            panic!("expected auth response")
        }
    };
    assert!(authorized["result"]["resources"].is_array());
}

#[tokio::test]
async fn sampling_and_elicitation_requests_return_boundary_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
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

    for (method, feature) in [
        (mcp_protocol::METHOD_SAMPLING_CREATE_MESSAGE, "sampling"),
        (mcp_protocol::METHOD_ELICITATION_CREATE, "elicitation"),
    ] {
        let response = mcp_response(
            &server,
            harn_vm::jsonrpc::request(2, method, json!({})),
            session.clone(),
        )
        .await;
        assert_eq!(response["error"]["code"], json!(-32601));
        assert_eq!(response["error"]["data"]["feature"], json!(feature));
        assert_eq!(response["error"]["data"]["role"], json!("client"));
    }
}

#[tokio::test]
async fn adapter_protocol_fixture_matches_checked_in_matrix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
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
    let templates_list = harn_vm::jsonrpc::request(5, "resources/templates/list", json!({}));

    let actual = vec![
        initialize.clone(),
        mcp_response(&server, initialize, session.clone()).await,
        harn_vm::jsonrpc::notification("notifications/initialized", json!({})),
        tools_list.clone(),
        mcp_response(&server, tools_list, session.clone()).await,
        resources_list.clone(),
        mcp_response(&server, resources_list, session.clone()).await,
        resources_read.clone(),
        mcp_response(&server, resources_read, session.clone()).await,
        templates_list.clone(),
        mcp_response(&server, templates_list, session).await,
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
async fn tool_call_rejects_task_augmentation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
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
