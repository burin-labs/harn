use super::*;
use crate::http::framing::{http_content_length_from_headers, TEST_HTTP_MAX_BODY_BYTES};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

mod conversion;
mod embedded_input;
mod http_fixtures;
mod oauth;
mod support;
use support::execute_test_harn;

#[derive(Debug)]
struct RecordedHttpRequest {
    headers: BTreeMap<String, String>,
    body: serde_json::Value,
}

struct CapturingAgentEventSink(Arc<std::sync::Mutex<Vec<crate::agent_events::AgentEvent>>>);

impl crate::agent_events::AgentEventSink for CapturingAgentEventSink {
    fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

struct CurrentHostBridgeGuard;

impl CurrentHostBridgeGuard {
    fn install() -> Self {
        let bridge = crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(|_| Ok(())),
            1,
        );
        crate::llm::install_current_host_bridge(Arc::new(bridge));
        Self
    }
}

impl Drop for CurrentHostBridgeGuard {
    fn drop(&mut self) {
        crate::llm::clear_current_host_bridge();
    }
}

struct SessionLifecycleGuard(String);

impl Drop for SessionLifecycleGuard {
    fn drop(&mut self) {
        crate::agent_sessions::close(&self.0);
    }
}

struct SamplingMockGuard;

impl Drop for SamplingMockGuard {
    fn drop(&mut self) {
        crate::llm::clear_cli_llm_mock_mode();
        crate::stdlib::host::reset_host_state();
    }
}

// Keep loopback HTTP server lifetimes isolated under the parallel Rust test
// harness so one test cannot consume another test's local networking resources.
async fn http_mcp_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

fn http_spec(url: &str, auth_token: Option<&str>) -> McpServerSpec {
    McpServerSpec {
        name: "mock-http".to_string(),
        transport: McpTransport::Http,
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: None,
        url: url.to_string(),
        auth_token: auth_token.map(str::to_string),
        token_exchange: None,
        protocol_version: None,
        proxy_server_name: None,
    }
}

#[test]
fn connect_protocol_options_accept_sdk_versions_and_reject_unknown_versions() {
    for version in crate::mcp_protocol::sdk_protocol_versions() {
        let options = resolve_connect_protocol_options(Some(version))
            .expect("SDK-supported protocol version should be accepted");
        assert_eq!(options.protocol_version, version);
    }

    let error = match resolve_connect_protocol_options(Some("2099-01-01")) {
        Ok(_) => panic!("unknown protocol version must fail locally"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("unsupported protocol_version"),
        "{error}"
    );
}

#[test]
fn connect_protocol_options_trim_version() {
    let options =
        resolve_connect_protocol_options(Some(" 2026-07-28 ")).expect("trimmed stable version");
    assert_eq!(options.protocol_version, PROTOCOL_VERSION);
}

#[tokio::test]
async fn http_auth_resolution_prefers_explicit_token() {
    let spec = http_spec("https://mcp.example/mcp", Some("configured"));
    let resolved = resolve_http_auth_token_source_with(&spec, |_| async {
        panic!("resolver must not run when the config carries a bearer token")
    })
    .await;
    assert_eq!(resolved.token.as_deref(), Some("configured"));
    assert_eq!(resolved.source, HttpAuthTokenSource::Config);
}

#[tokio::test]
async fn http_auth_resolution_uses_harn_store_when_config_omits_token() {
    let spec = http_spec("https://mcp.example/mcp", Some(""));
    let resolved = resolve_http_auth_token_source_with(&spec, |server_url| async move {
        assert_eq!(server_url, "https://mcp.example/mcp");
        Ok(Some("stored".to_string()))
    })
    .await;
    assert_eq!(resolved.token.as_deref(), Some("stored"));
    assert_eq!(resolved.source, HttpAuthTokenSource::OAuthStore);
}

#[tokio::test]
async fn http_auth_resolution_leaves_unauthenticated_servers_probeable() {
    let spec = http_spec("https://mcp.example/mcp", None);
    let resolved = resolve_http_auth_token_source_with(&spec, |_| async {
        Err("no protected-resource metadata".to_string())
    })
    .await;
    assert_eq!(resolved.token, None);
    assert_eq!(resolved.source, HttpAuthTokenSource::None);
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_stable_connect_uses_server_discover_with_metadata() {
    let script = r#"
import json, sys
request = json.loads(sys.stdin.readline())
assert request["method"] == "server/discover"
meta = request["params"]["_meta"]
assert meta["io.modelcontextprotocol/protocolVersion"] == "2026-07-28"
assert meta["io.modelcontextprotocol/clientInfo"]["name"] == "harn"
assert "io.modelcontextprotocol/clientCapabilities" in meta
print(json.dumps({
"jsonrpc": "2.0",
"id": request["id"],
"result": {
    "resultType": "complete",
    "supportedVersions": ["2026-07-28"],
    "capabilities": {"tools": {}},
    "ttlMs": 0,
    "cacheScope": "private",
    "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "stable", "version": "1.0.0"}}
}

}), flush=True)
"#;
    let handle = connect_stdio_test_script(script, PROTOCOL_VERSION.to_string()).await;
    let discovery = handle.discovery_result.lock().await.clone().unwrap();
    assert_eq!(
        discovery["protocolVersion"],
        serde_json::json!(PROTOCOL_VERSION)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_server_uses_configured_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let script = r#"
import json, os, sys
request = json.loads(sys.stdin.readline())
print(json.dumps({
    "jsonrpc": "2.0",
    "id": request["id"],
    "result": {
        "resultType": "complete",
        "supportedVersions": ["2026-07-28"],
        "capabilities": {"tools": {}},
        "ttlMs": 0,
        "cacheScope": "private",
        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": os.getcwd(), "version": "1.0.0"}}
    }
}), flush=True)
"#;
    let args = vec!["-u".to_string(), "-c".to_string(), script.to_string()];
    let handle = mcp_connect_stdio_impl(
        "python3",
        &args,
        &BTreeMap::new(),
        directory.path().to_str(),
        PROTOCOL_VERSION.to_string(),
    )
    .await
    .expect("stdio server should launch from configured cwd");
    let discovery = handle.discovery_result.lock().await.clone().unwrap();
    let reported_directory = discovery["serverInfo"]["name"]
        .as_str()
        .expect("server name should contain its working directory");
    assert_eq!(
        std::path::Path::new(reported_directory)
            .canonicalize()
            .expect("reported working directory should exist"),
        directory.path().canonicalize().unwrap(),
        "stdio server should observe the configured working directory"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_client_falls_back_to_initialize_with_each_sdk_released_version() {
    let script = r#"
import json, sys
discover = json.loads(sys.stdin.readline())
assert discover["method"] == "server/discover"
requested = discover["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
print(json.dumps({
    "jsonrpc": "2.0",
    "id": discover["id"],
    "error": {"code": -32601, "message": "Method not found"}
}), flush=True)
initialize = json.loads(sys.stdin.readline())
assert initialize["method"] == "initialize"
assert initialize["params"]["protocolVersion"] == requested
print(json.dumps({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "protocolVersion": requested,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "released", "version": "1.0.0"}
    }
}), flush=True)
initialized = json.loads(sys.stdin.readline())
assert initialized["method"] == "notifications/initialized"
"#;

    for version in rmcp::model::ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .filter(|version| *version < &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
    {
        let handle = connect_stdio_test_script(script, version.as_str().to_string()).await;
        let peer = handle.discovery_result.lock().await.clone().unwrap();
        assert_eq!(peer["protocolVersion"], serde_json::json!(version.as_str()));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_client_preserves_fields_outside_the_current_sdk_tool_model() {
    let script = r#"
import json, sys
discover = json.loads(sys.stdin.readline())
print(json.dumps({
    "jsonrpc": "2.0",
    "id": discover["id"],
    "error": {"code": -32601, "message": "Method not found"}
}), flush=True)
initialize = json.loads(sys.stdin.readline())
print(json.dumps({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "versioned-fields", "version": "1.0.0"}
    }
}), flush=True)
initialized = json.loads(sys.stdin.readline())
assert initialized["method"] == "notifications/initialized"
while True:
    request = json.loads(sys.stdin.readline())
    if request["method"] == "tools/list":
        break
print(json.dumps({
    "jsonrpc": "2.0",
    "id": request["id"],
    "result": {
        "tools": [{
            "name": "long_task",
            "inputSchema": {"type": "object"},
            "execution": {"taskSupport": "optional"},
            "x-future-display": {"density": "compact"}
        }]
    }
}), flush=True)
"#;
    let handle = connect_stdio_test_script(script, "2025-11-25".to_string()).await;
    let result = handle
        .call("tools/list", serde_json::json!({}))
        .await
        .expect("tools/list should preserve the negotiated wire result");

    assert_eq!(
        result["tools"][0]["execution"]["taskSupport"],
        serde_json::json!("optional")
    );
    assert_eq!(
        result["tools"][0]["x-future-display"]["density"],
        serde_json::json!("compact")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_sends_stateless_metadata_headers_and_schema_headers() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;

            let tools_result = handle
                .call("tools/list", serde_json::json!({}))
                .await
                .unwrap();
            handle.record_cache_hint("tools/list", &tools_result).await;
            let tools = filter_tools_for_client(
                &tools_result["tools"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            handle.store_http_tool_headers(&tools).await;
            assert_eq!(
                handle.cache_hints.lock().await.get("tools/list"),
                Some(&McpCacheHint {
                    ttl_ms: Some(300_000),
                    scope: Some("public"),
                })
            );

            let call_result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            assert_eq!(call_result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let list = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&list, "tools/list", None);
            let tool_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&tool_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                tool_call
                    .headers
                    .get("mcp-param-region")
                    .map(String::as_str),
                Some("us-west1")
            );
            assert!(!tool_call.headers.contains_key("mcp-session-id"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_input_required_result_dispatches_and_retries() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let session_id = crate::agent_sessions::open_or_create_for_test(Some(
                "mcp-input-required".to_string(),
            ));
            let _session_lifecycle = SessionLifecycleGuard(session_id.clone());
            let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
            let captured_events = install_capturing_agent_sink(&session_id);
            let _sampling_mock = install_sampling_mock().await;
            let result = call_mcp_tool(
                &handle,
                "needs_input",
                serde_json::json!({"prompt": "continue"}),
            )
            .await
            .unwrap();
            assert_eq!(result, serde_json::json!("done"));

            let _discover = recv_recorded_request(&mut requests).await;
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("needs_input"));
            assert!(first_call.body["params"].get("inputResponses").is_none());

            let retry_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&retry_call, "tools/call", Some("needs_input"));
            let responses = &retry_call.body["params"]["inputResponses"];
            assert!(responses["roots"]["roots"].as_array().is_some());
            assert_eq!(
                responses["elicitation"]["action"],
                serde_json::json!("decline")
            );
            assert_eq!(
                responses["sampling"]["content"]["text"],
                serde_json::json!("sampled")
            );
            assert_eq!(
                retry_call.body["params"]["requestState"],
                serde_json::json!("state-1")
            );

            // The schema belongs to the embedded input request and is not
            // echoed in the protocol retry. This event is emitted after the
            // production InputRequests decode and re-serialization boundary;
            // the retry assertions above prove that the same round completed.
            let events = captured_events.lock().unwrap();
            let elicitation_requests = events
                .iter()
                .filter_map(|event| match event {
                    crate::agent_events::AgentEvent::McpNotification {
                        server,
                        method,
                        direction,
                        params,
                        ..
                    } if server == "stable-http"
                        && method == crate::mcp_elicit::ELICITATION_METHOD
                        && direction == "request" =>
                    {
                        Some(params)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                elicitation_requests.len(),
                1,
                "expected one decoded elicitation request, got {events:?}"
            );
            let requested_schema = &elicitation_requests[0]["requestedSchema"];
            assert_eq!(
                requested_schema["$schema"],
                serde_json::json!("https://json-schema.org/draft/2020-12/schema")
            );
            assert_eq!(
                elicitation_requests[0]["requestedSchemaPropertyOrder"],
                serde_json::json!(["zeta", "alpha"]),
                "typed RMCP conversion must project declared property order explicitly"
            );
            assert_eq!(
                requested_schema["properties"]["zeta"]["type"],
                serde_json::json!("string")
            );
            assert_eq!(
                requested_schema["properties"]["alpha"]["type"],
                serde_json::json!("integer")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_retries_prompt_and_resource_input_rounds() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let _discover = recv_recorded_request(&mut requests).await;

            for (method, params, marker, expected_name) in [
                (
                    "prompts/get",
                    serde_json::json!({"name": "review", "arguments": {}}),
                    "prompt-complete",
                    Some("review"),
                ),
                (
                    "resources/read",
                    serde_json::json!({"uri": "file:///fixture"}),
                    "resource-complete",
                    Some("file:///fixture"),
                ),
            ] {
                let result = handle.call(method, params).await.unwrap();
                assert_eq!(result["description"], serde_json::json!(marker));

                let first = recv_recorded_request(&mut requests).await;
                assert_stable_http_request(&first, method, expected_name);
                assert!(first.body["params"].get("inputResponses").is_none());

                let retry = recv_recorded_request(&mut requests).await;
                assert_stable_http_request(&retry, method, expected_name);
                assert!(retry.body["params"]["inputResponses"]["roots"]["roots"]
                    .as_array()
                    .is_some());
                assert_eq!(
                    retry.body["params"]["requestState"],
                    serde_json::json!("state-non-tool")
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_task_result_is_polled_to_completion() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let result = call_mcp_tool(&handle, "deferred", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(result, serde_json::json!("completed asynchronously"));

            let _discover = recv_recorded_request(&mut requests).await;
            let create = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&create, "tools/call", Some("deferred"));
            let poll = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&poll, "tasks/get", Some("task-1"));
            assert_eq!(poll.body["params"]["taskId"], serde_json::json!("task-1"));
        })
        .await;
}

#[test]
fn x_mcp_header_validation_filters_invalid_tools_and_encodes_values() {
    let tools = vec![
        serde_json::json!({
            "name": "valid",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "region": {"type": "string", "x-mcp-header": "Region"}
                }
            }
        }),
        serde_json::json!({
            "name": "invalid",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "body": {"type": "object", "x-mcp-header": "Body"}
                }
            }
        }),
    ];
    let filtered = filter_tools_for_client(&tools);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["name"], serde_json::json!("valid"));
    assert_eq!(
        encode_mcp_header_value(&serde_json::json!("Hello, 世界")).unwrap(),
        "=?base64?SGVsbG8sIOS4lueVjA==?="
    );
}

pub(super) async fn connect_stdio_test_script(
    script: &str,
    protocol_version: String,
) -> VmMcpClientHandle {
    let args = vec!["-u".to_string(), "-c".to_string(), script.to_string()];
    mcp_connect_stdio_impl("python3", &args, &BTreeMap::new(), None, protocol_version)
        .await
        .expect("stdio test MCP server should connect")
}

async fn arm_sampling_mock() {
    crate::llm::install_cli_llm_mocks(vec![crate::llm::parse_llm_mock_value(
        &serde_json::json!({"text": "sampled", "provider": "mock", "model": "mock"}),
    )
    .expect("sampling mock")]);
    execute_test_harn(
        r#"
host_mock("mcp", "sample", {
  result: {
    action: "accept",
    options: {provider: "mock", model: "mock"},
  },
  unregistered_ok: true
})
"#,
    )
    .await;
}

async fn install_sampling_mock() -> SamplingMockGuard {
    arm_sampling_mock().await;
    SamplingMockGuard
}

async fn stable_http_handle(base_url: &str) -> VmMcpClientHandle {
    let spec = McpServerSpec {
        name: "stable-http".to_string(),
        transport: McpTransport::Http,
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: None,
        url: format!("{base_url}/mcp"),
        auth_token: None,
        token_exchange: None,
        protocol_version: Some(PROTOCOL_VERSION.to_string()),
        proxy_server_name: None,
    };
    connect_mcp_server_from_spec(&spec)
        .await
        .expect("stable HTTP MCP server should connect")
}

fn assert_stable_http_request(request: &RecordedHttpRequest, method: &str, name: Option<&str>) {
    assert_eq!(request.body["method"], serde_json::json!(method));
    assert_eq!(
        request
            .headers
            .get("mcp-protocol-version")
            .map(String::as_str),
        Some(PROTOCOL_VERSION)
    );
    assert_eq!(
        request.headers.get("mcp-method").map(String::as_str),
        Some(method)
    );
    assert_eq!(request.headers.get("mcp-name").map(String::as_str), name);
    assert!(!request.headers.contains_key("mcp-session-id"));
    let meta = &request.body["params"]["_meta"];
    assert_eq!(
        meta[MCP_META_KEY_PROTOCOL_VERSION],
        serde_json::json!(PROTOCOL_VERSION)
    );
    assert_eq!(
        meta[MCP_META_KEY_CLIENT_INFO]["name"],
        serde_json::json!("harn")
    );
    assert_eq!(
        meta[MCP_META_KEY_CLIENT_CAPABILITIES]["roots"],
        serde_json::json!({})
    );
}

async fn spawn_stable_http_mcp_server() -> (String, mpsc::UnboundedReceiver<RecordedHttpRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok((_request_line, headers, body)) = read_http_request(&mut stream).await else {
                continue;
            };
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                continue;
            };
            let _ = request_tx.send(RecordedHttpRequest {
                headers: headers.clone(),
                body: request.clone(),
            });
            let method = request.get("method").and_then(|value| value.as_str());
            if stable_http_needs_input_round(&request) {
                let body = stable_http_input_required_body(
                    request.get("id").unwrap_or(&serde_json::Value::Null),
                );
                let _ = write_http_json_text(&mut stream, "200 OK", &[], &body).await;
            } else {
                let response = stable_http_response(&request, method);
                let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
            }
        }
    });

    (format!("http://{addr}"), request_rx)
}

fn install_capturing_agent_sink(
    session_id: &str,
) -> Arc<std::sync::Mutex<Vec<crate::agent_events::AgentEvent>>> {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    crate::agent_events::register_sink(
        session_id.to_string(),
        Arc::new(CapturingAgentEventSink(events.clone())),
    );
    events
}

#[test]
fn list_changed_notification_emits_catalog_changed_cue() {
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("mcp-list-changed".to_string()));
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let captured = install_capturing_agent_sink(&session_id);

    // A `tools/list_changed` notification emits the catalog-change cue so a thin
    // client re-fetches the catalog and surfaces the new tools this session.
    super::notifications::relay_resource_notification(
        "calc",
        "notifications/tools/list_changed",
        &serde_json::json!({ "method": "notifications/tools/list_changed" }),
    );
    // A content-only `resources/updated` is not a catalog change and must not.
    super::notifications::relay_resource_notification(
        "calc",
        "notifications/resources/updated",
        &serde_json::json!({ "method": "notifications/resources/updated" }),
    );

    let events = captured.lock().unwrap().clone();
    let catalog_changed: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                crate::agent_events::AgentEvent::McpCatalogChanged { .. }
            )
        })
        .collect();
    assert_eq!(
        catalog_changed.len(),
        1,
        "exactly one catalog-changed cue for the single list_changed notification"
    );
    match catalog_changed[0] {
        crate::agent_events::AgentEvent::McpCatalogChanged { server, reason, .. } => {
            assert_eq!(server.as_deref(), Some("calc"));
            assert_eq!(reason, "list_changed");
        }
        _ => unreachable!(),
    }
}

async fn recv_recorded_request(
    requests: &mut mpsc::UnboundedReceiver<RecordedHttpRequest>,
) -> RecordedHttpRequest {
    tokio::time::timeout(MCP_TIMEOUT, async {
        loop {
            let request = requests
                .recv()
                .await
                .expect("mock server closed before recording request");
            if request.body.get("id").is_some() {
                return request;
            }
        }
    })
    .await
    .expect("timed out waiting for recorded MCP HTTP request")
}

fn stable_http_response(request: &serde_json::Value, method: Option<&str>) -> serde_json::Value {
    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match method {
        Some("server/discover") => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "supportedVersions": [PROTOCOL_VERSION],
                "capabilities": {"tools": {}, "resources": {}},
                "ttlMs": 0,
                "cacheScope": "private",
                "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "stable-http", "version": "1.0.0"}}
            }
        }),
        Some("tools/list") => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "tools": [{
                    "name": "execute_sql",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "region": {"type": "string", "x-mcp-header": "Region"},
                            "query": {"type": "string"}
                        },
                        "required": ["region", "query"]
                    }
                }],
                "ttlMs": 300000,
                "cacheScope": "public"
            }
        }),
        Some("tools/call") => stable_http_tool_call_response(request, id),
        Some(method @ ("prompts/get" | "resources/read")) => {
            stable_http_non_tool_input_response(request, id, method)
        }
        Some("tasks/get") => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "taskId": "task-1",
                "status": "completed",
                "result": {
                    "resultType": "complete",
                    "content": [{"type": "text", "text": "completed asynchronously"}],
                    "isError": false
                }
            }
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "Method not found"}
        }),
    }
}

fn stable_http_non_tool_input_response(
    request: &serde_json::Value,
    id: serde_json::Value,
    method: &str,
) -> serde_json::Value {
    if request["params"].get("inputResponses").is_none() {
        return serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "input_required",
                "requestState": "state-non-tool",
                "inputRequests": {
                    "roots": {"method": "roots/list", "params": {}}
                }
            }
        });
    }
    let marker = match method {
        "prompts/get" => "prompt-complete",
        "resources/read" => "resource-complete",
        _ => unreachable!("caller restricts stable non-tool input methods"),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "resultType": "complete",
            "description": marker
        }
    })
}

fn stable_http_tool_call_response(
    request: &serde_json::Value,
    id: serde_json::Value,
) -> serde_json::Value {
    let params = &request["params"];
    let name = params.get("name").and_then(|value| value.as_str());
    if name == Some("deferred") {
        return serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "task",
                "taskId": "task-1",
                "pollIntervalMs": 10
            }
        });
    }
    let text = if name == Some("needs_input") {
        "done"
    } else {
        "ok"
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "resultType": "complete",
            "content": [{
                "type": "text",
                "text": text
            }],
            "isError": false
        }
    })
}

fn stable_http_needs_input_round(request: &serde_json::Value) -> bool {
    request.get("method").and_then(serde_json::Value::as_str) == Some("tools/call")
        && request["params"].get("inputResponses").is_none()
        && request["params"]
            .get("name")
            .and_then(serde_json::Value::as_str)
            == Some("needs_input")
}

fn stable_http_input_required_body(id: &serde_json::Value) -> String {
    let id = serde_json::to_string(id).expect("JSON-RPC id is JSON");
    format!(
        r#"{{
  "jsonrpc": "2.0",
  "id": {id},
  "result": {{
    "resultType": "input_required",
    "requestState": "state-1",
    "inputRequests": {{
      "roots": {{"method": "roots/list", "params": {{}}}},
      "elicitation": {{
        "method": "elicitation/create",
        "params": {{
          "mode": "form",
          "message": "Need input",
          "requestedSchema": {{
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {{
              "zeta": {{"type": "string"}},
              "alpha": {{"type": "integer"}}
            }}
          }}
        }}
      }},
      "sampling": {{
        "method": "sampling/createMessage",
        "params": {{
          "messages": [{{
            "role": "user",
            "content": {{"type": "text", "text": "sample"}}
          }}],
          "maxTokens": 4
        }}
      }}
    }}
  }}
}}"#
    )
}

async fn spawn_recording_http_mcp_server() -> (String, mpsc::UnboundedReceiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok((_request_line, _headers, body)) = read_http_request(&mut stream).await else {
                continue;
            };
            if let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) {
                let _ = request_tx.send(request);
            }
            let _ = write_http_empty(&mut stream, "202 Accepted").await;
        }
    });

    (format!("http://{addr}"), request_rx)
}

async fn read_http_request(
    stream: &mut TcpStream,
) -> Result<(String, BTreeMap<String, String>, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let bytes = stream.read(&mut chunk).await?;
        if bytes == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("missing HTTP header terminator")?;
    let header_text = String::from_utf8(buffer[..header_end].to_vec())?;
    let mut lines = header_text.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = http_content_length_from_headers(&headers, TEST_HTTP_MAX_BODY_BYTES)?;
    let mut body = buffer[header_end + 4..].to_vec();
    let mut chunk = [0_u8; 8192];
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let read_len = remaining.min(chunk.len());
        let bytes = stream.read(&mut chunk[..read_len]).await?;
        if bytes == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..bytes]);
    }
    body.truncate(content_length);
    Ok((request_line, headers, body))
}

#[tokio::test(flavor = "current_thread")]
async fn read_http_request_rejects_oversized_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let request = format!(
            "POST /mcp HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
            TEST_HTTP_MAX_BODY_BYTES + 1
        );
        stream.write_all(request.as_bytes()).await.expect("write");
    });

    let (mut stream, _) = listener.accept().await.expect("accept");
    let error = read_http_request(&mut stream)
        .await
        .expect_err("oversized content length should be rejected");
    assert!(error.to_string().contains("exceeds limit"));
    client.await.expect("client task");
}

async fn write_http_json(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: serde_json::Value,
) -> Result<(), std::io::Error> {
    let body = serde_json::to_string(&body).unwrap();
    write_http_json_text(stream, status, headers, &body).await
}

async fn write_http_json_text(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<(), std::io::Error> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    response.push_str(body);
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

async fn write_http_empty(stream: &mut TcpStream, status: &str) -> Result<(), std::io::Error> {
    let response = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}
