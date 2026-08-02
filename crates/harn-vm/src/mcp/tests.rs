use super::*;
use crate::http::framing::{http_content_length_from_headers, TEST_HTTP_MAX_BODY_BYTES};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

mod conversion;
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
        url: url.to_string(),
        auth_token: auth_token.map(str::to_string),
        token_exchange: None,
        protocol_version: None,
        proxy_server_name: None,
    }
}

#[test]
fn connect_protocol_options_reject_non_stable_versions() {
    for version in ["2025-11-25", "2099-01-01"] {
        let error = match resolve_connect_protocol_options(Some(version)) {
            Ok(_) => panic!("non-stable protocol version must fail locally: {version}"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("unsupported protocol_version"),
            "{error}"
        );
    }
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
            install_sampling_mock().await;
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
            clear_sampling_mock().await;
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
            assert_stable_http_request(&poll, "tasks/get", None);
            assert_eq!(poll.body["params"]["taskId"], serde_json::json!("task-1"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_401_auth_required_waits_for_oauth_and_retries_tool_call() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, auth_challenged) =
                spawn_auth_required_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let server_url = format!("{base_url}/mcp");
            let resource = crate::mcp_auth::canonical_resource_indicator(&server_url).unwrap();
            let session_id =
                crate::agent_sessions::open_or_create(Some("mcp-auth-retry".to_string()));
            let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
            let _bridge_guard = CurrentHostBridgeGuard::install();
            let captured_events = install_capturing_agent_sink(&session_id);

            let notifier = tokio::spawn({
                let resource = resource.clone();
                async move {
                    auth_challenged
                        .await
                        .expect("mock server should issue an auth challenge");
                    let token = test_stored_mcp_token(&resource, "fresh-token");
                    crate::mcp_oauth::notify_authorization_completed(&token);
                }
            });

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            notifier.await.expect("auth notifier task should complete");
            assert_eq!(result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("execute_sql"));
            assert!(!first_call.headers.contains_key("authorization"));
            let retry_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&retry_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                retry_call.headers.get("authorization").map(String::as_str),
                Some("Bearer fresh-token")
            );

            let events = captured_events.lock().unwrap().clone();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    crate::agent_events::AgentEvent::McpAuthRequired {
                        session_id: event_session_id,
                        server,
                        resource: event_resource,
                        scope: Some(scope),
                    } if event_session_id == &session_id
                        && server == "stable-http"
                        && event_resource == &resource
                        && scope == "repo"
                )),
                "expected McpAuthRequired event, got {events:?}"
            );
            crate::agent_events::clear_session_sinks(&session_id);
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_403_insufficient_scope_waits_for_step_up_and_retries_tool_call() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, auth_challenged) =
                spawn_insufficient_scope_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let server_url = format!("{base_url}/mcp");
            let resource = crate::mcp_auth::canonical_resource_indicator(&server_url).unwrap();
            let session_id =
                crate::agent_sessions::open_or_create(Some("mcp-scope-stepup".to_string()));
            let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
            let _bridge_guard = CurrentHostBridgeGuard::install();
            let captured_events = install_capturing_agent_sink(&session_id);

            let notifier = tokio::spawn({
                let resource = resource.clone();
                async move {
                    auth_challenged
                        .await
                        .expect("mock server should issue an insufficient_scope challenge");
                    let token = test_stored_mcp_token(&resource, "fresh-token");
                    crate::mcp_oauth::notify_authorization_completed(&token);
                }
            });

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            notifier.await.expect("auth notifier task should complete");
            assert_eq!(result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("execute_sql"));
            let retry_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&retry_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                retry_call.headers.get("authorization").map(String::as_str),
                Some("Bearer fresh-token")
            );

            let events = captured_events.lock().unwrap().clone();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    crate::agent_events::AgentEvent::McpAuthRequired {
                        session_id: event_session_id,
                        server,
                        resource: event_resource,
                        scope: Some(scope),
                    } if event_session_id == &session_id
                        && server == "stable-http"
                        && event_resource == &resource
                        // The step-up event carries the elevated scope from the
                        // insufficient_scope challenge, not just the base scope.
                        && scope == "repo admin"
                )),
                "expected McpAuthRequired step-up event with elevated scope, got {events:?}"
            );
            crate::agent_events::clear_session_sinks(&session_id);
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_401_without_interactive_host_returns_auth_error() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            crate::llm::clear_current_host_bridge();
            let (base_url, mut requests, _auth_challenged) =
                spawn_auth_required_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let session_id =
                crate::agent_sessions::open_or_create(Some("mcp-auth-headless".to_string()));
            let _session_guard = crate::agent_sessions::enter_current_session(session_id);
            let error = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .expect_err("headless MCP auth challenge should fail clearly");

            match error {
                VmError::CategorizedError { category, message } => {
                    assert_eq!(category, crate::value::ErrorCategory::Auth);
                    assert!(message.contains("stable-http"), "{message}");
                    assert!(message.contains("no interactive host"), "{message}");
                }
                other => panic!("expected categorized auth error, got {other:?}"),
            }

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("execute_sql"));
            assert!(
                matches!(requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "headless path should not retry"
            );
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_token_exchange_sends_delegated_bearer_for_actor_chain() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, mut exchanges) =
                spawn_token_exchange_http_mcp_server(true, "Bearer delegated-token").await;
            let spec = token_exchange_http_spec(&base_url);
            let handle = connect_mcp_server_from_spec(&spec)
                .await
                .expect("stable HTTP MCP server should connect");
            let actor_chain =
                crate::actor_chain::ActorChain::new_with_scopes("user:kenneth", ["repo"])
                    .pushed_with_scopes("agent:merge-captain", ["repo"]);
            let session_id = crate::agent_sessions::open_or_create_with_actor_chain(
                Some("mcp-token-exchange".to_string()),
                Some(actor_chain),
            );
            let _session_guard = crate::agent_sessions::enter_current_session(session_id);

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            assert_eq!(result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            assert_eq!(
                discover.headers.get("authorization").map(String::as_str),
                Some("Bearer base-token")
            );
            let tool_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&tool_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                tool_call.headers.get("authorization").map(String::as_str),
                Some("Bearer delegated-token")
            );

            let exchange = recv_token_exchange_form(&mut exchanges).await;
            assert_eq!(
                exchange.get("grant_type").map(String::as_str),
                Some("urn:ietf:params:oauth:grant-type:token-exchange")
            );
            assert_eq!(
                exchange.get("subject_token").map(String::as_str),
                Some("base-token")
            );
            assert_eq!(
                exchange.get("subject_token_type").map(String::as_str),
                Some("urn:ietf:params:oauth:token-type:access_token")
            );
            assert_eq!(
                exchange.get("actor_token").map(String::as_str),
                Some("agent.jwt")
            );
            assert_eq!(
                exchange.get("actor_token_type").map(String::as_str),
                Some("urn:ietf:params:oauth:token-type:jwt")
            );
            assert_eq!(exchange.get("scope").map(String::as_str), Some("repo"));
            let expected_resource = format!("{base_url}/mcp");
            assert_eq!(
                exchange.get("resource").map(String::as_str),
                Some(expected_resource.as_str())
            );
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_token_exchange_unsupported_grant_falls_back_to_plain_bearer() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, mut exchanges) =
                spawn_token_exchange_http_mcp_server(false, "Bearer base-token").await;
            let spec = token_exchange_http_spec(&base_url);
            let handle = connect_mcp_server_from_spec(&spec)
                .await
                .expect("stable HTTP MCP server should connect");
            let actor_chain =
                crate::actor_chain::ActorChain::new("user:kenneth").pushed("agent:merge-captain");
            let session_id = crate::agent_sessions::open_or_create_with_actor_chain(
                Some("mcp-token-exchange-fallback".to_string()),
                Some(actor_chain),
            );
            let _session_guard = crate::agent_sessions::enter_current_session(session_id);

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            assert_eq!(result, serde_json::json!("ok"));

            let _discover = recv_recorded_request(&mut requests).await;
            let tool_call = recv_recorded_request(&mut requests).await;
            assert_eq!(
                tool_call.headers.get("authorization").map(String::as_str),
                Some("Bearer base-token")
            );
            let exchange = recv_token_exchange_form(&mut exchanges).await;
            assert_eq!(
                exchange.get("grant_type").map(String::as_str),
                Some("urn:ietf:params:oauth:grant-type:token-exchange")
            );
            handle.disconnect().await.unwrap();
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
    mcp_connect_stdio_impl("python3", &args, &BTreeMap::new(), protocol_version)
        .await
        .expect("stdio test MCP server should connect")
}

async fn install_sampling_mock() {
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

async fn clear_sampling_mock() {
    crate::llm::clear_cli_llm_mock_mode();
    execute_test_harn("host_mock_clear()").await;
}

async fn stable_http_handle(base_url: &str) -> VmMcpClientHandle {
    let spec = McpServerSpec {
        name: "stable-http".to_string(),
        transport: McpTransport::Http,
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
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

fn token_exchange_http_spec(base_url: &str) -> McpServerSpec {
    McpServerSpec {
        name: "token-exchange-http".to_string(),
        transport: McpTransport::Http,
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        url: format!("{base_url}/mcp"),
        auth_token: Some("base-token".to_string()),
        token_exchange: Some(crate::mcp_oauth::McpTokenExchangeConfig {
            token_url: Some(format!("{base_url}/token")),
            actor_token: Some("agent.jwt".to_string()),
            ..Default::default()
        }),
        protocol_version: Some(PROTOCOL_VERSION.to_string()),
        proxy_server_name: None,
    }
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
            let response = stable_http_response(&request, method);
            let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
        }
    });

    (format!("http://{addr}"), request_rx)
}

async fn spawn_auth_required_stable_http_mcp_server() -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    oneshot::Receiver<()>,
) {
    // A `401 Unauthorized` with a Bearer challenge: no/invalid token.
    spawn_challenge_then_ok_stable_http_mcp_server("401 Unauthorized", r#"Bearer scope="repo""#)
        .await
}

async fn spawn_insufficient_scope_stable_http_mcp_server() -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    oneshot::Receiver<()>,
) {
    // A `403 Forbidden` with `error="insufficient_scope"`: a valid token that
    // lacks a required scope. Resolvable by a step-up authorization requesting
    // the elevated `scope` from the challenge.
    spawn_challenge_then_ok_stable_http_mcp_server(
        "403 Forbidden",
        r#"Bearer error="insufficient_scope", scope="repo admin""#,
    )
    .await
}

/// Stable-HTTP MCP mock that answers the first `tools/call` (and any call
/// without a `Bearer fresh-token`) with `status_line` + the given
/// `WWW-Authenticate` `challenge`, then serves `200 OK` once the fresh token
/// is presented. Used to exercise both the `401` and `403 insufficient_scope`
/// step-up authorization paths through one code path.
async fn spawn_challenge_then_ok_stable_http_mcp_server(
    status_line: &'static str,
    challenge: &'static str,
) -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    oneshot::Receiver<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (auth_challenged_tx, auth_challenged_rx) = oneshot::channel();

    tokio::spawn(async move {
        let mut challenged = false;
        let mut auth_challenged_tx = Some(auth_challenged_tx);
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
            if method == Some("tools/call") && !challenged {
                challenged = true;
                if let Some(sender) = auth_challenged_tx.take() {
                    let _ = sender.send(());
                }
                let _ = write_http_json(
                    &mut stream,
                    status_line,
                    &[("WWW-Authenticate", challenge)],
                    serde_json::json!({"error": "authorization required"}),
                )
                .await;
                continue;
            }
            if method == Some("tools/call")
                && headers.get("authorization").map(String::as_str) != Some("Bearer fresh-token")
            {
                let _ = write_http_json(
                    &mut stream,
                    status_line,
                    &[("WWW-Authenticate", challenge)],
                    serde_json::json!({"error": "authorization required"}),
                )
                .await;
                continue;
            }
            let response = stable_http_response(&request, method);
            let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
        }
    });

    (format!("http://{addr}"), request_rx, auth_challenged_rx)
}

async fn spawn_token_exchange_http_mcp_server(
    exchange_supported: bool,
    expected_tool_authorization: &'static str,
) -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    mpsc::UnboundedReceiver<BTreeMap<String, String>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (exchange_tx, exchange_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok((request_line, headers, body)) = read_http_request(&mut stream).await else {
                continue;
            };
            if request_line.starts_with("POST /token ") {
                let form = url::form_urlencoded::parse(&body)
                    .into_owned()
                    .collect::<BTreeMap<_, _>>();
                let _ = exchange_tx.send(form);
                if exchange_supported {
                    let body = serde_json::json!({
                        "access_token": "delegated-token",
                        "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
                        "token_type": "Bearer",
                        "expires_in": 300,
                    });
                    let _ = write_http_json(&mut stream, "200 OK", &[], body).await;
                } else {
                    let body = serde_json::json!({"error": "unsupported_grant_type"});
                    let _ = write_http_json(&mut stream, "400 Bad Request", &[], body).await;
                }
                continue;
            }

            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                let _ = write_http_empty(&mut stream, "400 Bad Request").await;
                continue;
            };
            let _ = request_tx.send(RecordedHttpRequest {
                headers: headers.clone(),
                body: request.clone(),
            });
            let method = request.get("method").and_then(|value| value.as_str());
            if method == Some("tools/call")
                && headers.get("authorization").map(String::as_str)
                    != Some(expected_tool_authorization)
            {
                let _ = write_http_json(
                    &mut stream,
                    "401 Unauthorized",
                    &[("WWW-Authenticate", r#"Bearer scope="repo""#)],
                    serde_json::json!({"error": "authorization required"}),
                )
                .await;
                continue;
            }
            let response = stable_http_response(&request, method);
            let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
        }
    });

    (format!("http://{addr}"), request_rx, exchange_rx)
}

async fn recv_token_exchange_form(
    exchanges: &mut mpsc::UnboundedReceiver<BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    tokio::time::timeout(MCP_TIMEOUT, exchanges.recv())
        .await
        .expect("timed out waiting for token exchange request")
        .expect("mock server closed before recording token exchange")
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
    let session_id = crate::agent_sessions::open_or_create(Some("mcp-list-changed".to_string()));
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

fn test_stored_mcp_token(resource: &str, access_token: &str) -> crate::mcp_oauth::StoredMcpToken {
    crate::mcp_oauth::StoredMcpToken {
        access_token: access_token.to_string(),
        refresh_token: None,
        expires_at_unix: None,
        token_endpoint: "https://auth.example/token".to_string(),
        client_id: "test-client".to_string(),
        client_secret: None,
        token_endpoint_auth_method: "none".to_string(),
        issuer: "https://auth.example".to_string(),
        resource: resource.to_string(),
        scopes: Some("repo".to_string()),
        token_response_extra: None,
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
    if name == Some("needs_input") && params.get("inputResponses").is_none() {
        return serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "input_required",
                "requestState": "state-1",
                "inputRequests": {
                    "roots": {"method": "roots/list", "params": {}},
                    "elicitation": {
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "message": "Need input",
                            "requestedSchema": {
                                "type": "object",
                                "properties": {"answer": {"type": "string"}}
                            }
                        }
                    },
                    "sampling": {
                        "method": "sampling/createMessage",
                        "params": {
                            "messages": [{
                                "role": "user",
                                "content": {"type": "text", "text": "sample"}
                            }],
                            "maxTokens": 4
                        }
                    }
                }
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
    response.push_str(&body);
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

async fn write_http_empty(stream: &mut TcpStream, status: &str) -> Result<(), std::io::Error> {
    let response = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

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
