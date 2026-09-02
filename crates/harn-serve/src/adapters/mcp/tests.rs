use super::auth::should_stream_post_response;
use super::*;
use crate::{ApiKeyAuthConfig, AuthMethodConfig, DispatchCoreConfig};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

struct McpMutationConfigurator {
    calls: Arc<AtomicUsize>,
}

impl crate::VmConfigurator for McpMutationConfigurator {
    fn configure(&self, vm: &mut harn_vm::Vm) -> Result<(), DispatchError> {
        let calls = self.calls.clone();
        vm.register_builtin("test_increment_call_count", move |_args, _output| {
            let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(harn_vm::VmValue::Int(
                count.try_into().expect("test call count fits in i64"),
            ))
        });
        Ok(())
    }
}

struct McpRendezvousConfigurator {
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    releases: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>>,
}

impl crate::VmConfigurator for McpRendezvousConfigurator {
    fn configure(&self, vm: &mut harn_vm::Vm) -> Result<(), DispatchError> {
        let entered = self.entered.clone();
        let releases = Arc::clone(&self.releases);
        vm.register_async_builtin("test_rendezvous", move |_ctx, args| {
            let entered = entered.clone();
            let releases = Arc::clone(&releases);
            let value = args.first().cloned().unwrap_or(harn_vm::VmValue::Nil);
            async move {
                let key = value.display();
                let release = releases
                    .lock()
                    .expect("release map poisoned")
                    .get(&key)
                    .cloned()
                    .expect("release registered");
                let _ = entered.send(key);
                release
                    .acquire()
                    .await
                    .expect("release stays open")
                    .forget();
                Ok(value)
            }
        });
        Ok(())
    }
}

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
    assert_eq!(
        tools["tools"][0],
        server
            .tool_catalog
            .mcp_tool(&server.tool_catalog.tools[0])
            .expect("MCP projection")
    );
    assert_eq!(tools["tools"][0]["name"], "greet");
    assert_eq!(tools["tools"][0]["title"], "greet");
    assert_eq!(
        tools["tools"][0]["description"],
        "Exported Harn function 'greet'."
    );
    // Undeclared hints stay off the wire. MCP's own defaults apply; inventing
    // "destructive" / "open-world" here would be a safety claim the script
    // never made.
    assert!(tools["tools"][0]["annotations"]
        .get("readOnlyHint")
        .is_none());
    assert!(tools["tools"][0]["annotations"]
        .get("destructiveHint")
        .is_none());
    assert!(tools["tools"][0]["annotations"]
        .get("openWorldHint")
        .is_none());
    assert_eq!(tools["tools"][0]["inputSchema"]["type"], "object");
    assert_eq!(tools["tools"][0]["outputSchema"]["type"], "object");
    assert_eq!(
        tools["tools"][0]["outputSchema"]["properties"]["result"]["type"],
        "string"
    );
    let output_schema = tools["tools"][0]["outputSchema"].clone();
    let response = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(
            1,
            "tools/call",
            json!({"name": "greet", "arguments": {"name": "Ada"}}),
        ),
        SharedSession::new(),
    )
    .await;
    assert_eq!(response["result"]["structuredContent"]["result"], "Ada");
    let validator = jsonschema::validator_for(&output_schema).expect("valid output schema");
    assert!(validator.is_valid(&response["result"]["structuredContent"]));
}

#[tokio::test]
async fn declared_throw_is_typed_call_result_and_not_a_jsonrpc_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub type LookupError = {variant: "NotFound", message: string, secret: string}

pub fn lookup() -> string throws LookupError {
  throw {variant: "NotFound", message: "PRIVATE-CUSTOMER-DIAGNOSTIC-123456", secret: "typed-detail"}
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let listed = server.tools_list_result(&json!({}));
    assert_eq!(
        listed["tools"][0]["_meta"][harn_vm::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
            ["errorSchema"]["properties"]["variant"]["const"],
        "NotFound"
    );

    let response = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(1, "tools/call", json!({"name": "lookup", "arguments": {}})),
        SharedSession::new(),
    )
    .await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"].get("structuredContent").is_none());
    assert_eq!(
        response["result"]["_meta"][harn_vm::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
            ["applicationError"]["data"]["variant"],
        "NotFound"
    );
    let content = response["result"]["content"][0]["text"]
        .as_str()
        .expect("safe text content");
    assert!(content.contains("declared application error"), "{content}");
    assert!(!content.contains("typed-detail"), "{content}");
    assert!(
        !content.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"),
        "{content}"
    );
}

#[tokio::test]
async fn advertised_nominal_output_schema_validates_structured_tool_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub struct Greeting {
  message: string
  tags: list<string>
}
pub enum Outcome<T> {
  Success(value: T)
  Failure(message: string)
}
pub fn greet(name: string) -> Outcome<Greeting> {
  return Outcome.Success(Greeting {message: "hello ${name}", tags: ["typed"]})
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let listed = server.tools_list_result(&json!({}));
    let output_schema = listed["tools"][0]["outputSchema"].clone();
    let response = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(
            1,
            "tools/call",
            json!({"name": "greet", "arguments": {"name": "Ada"}}),
        ),
        SharedSession::new(),
    )
    .await;
    let structured = &response["result"]["structuredContent"];

    assert_eq!(structured["enum"], "Outcome");
    assert_eq!(structured["variant"], "Success");
    assert_eq!(structured["fields"][0]["message"], "hello Ada");
    assert_eq!(
        output_schema["oneOf"][0]["properties"]["variant"]["const"],
        "Success"
    );
    let validator = jsonschema::validator_for(&output_schema).expect("valid output schema");
    assert!(
        validator.is_valid(structured),
        "structuredContent must satisfy the exact tools/list outputSchema: {structured}"
    );
    assert!(
        !validator.is_valid(&json!({
            "enum": "Outcome",
            "variant": "Success",
            "fields": [{"message": 7, "tags": ["typed"]}],
        })),
        "the advertised schema must reject a value that violates the nested nominal type"
    );
}

#[tokio::test]
async fn tools_list_projects_doc_comments_and_declared_hints() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
/**
 * Eval debugger
 *
 * Start with find_runs.
 */
@annotations(readOnly: true, idempotent: true, openWorld: false)
/// Find eval runs
///
/// Lists recent evals. Does not start one.
pub fn find_runs() -> string {
  return "ok"
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let tools = server.tools_list_result(&json!({}));
    let tool = &tools["tools"][0];
    assert_eq!(tool["name"], "find_runs");
    assert_eq!(tool["title"], "Find eval runs");
    assert!(tool["description"]
        .as_str()
        .unwrap()
        .contains("Lists recent evals"));
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["idempotentHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);
    assert!(tool["annotations"].get("destructiveHint").is_none());

    let session = SharedSession::new();
    let initialized = match server
        .process_message(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "codex-mcp-client", "version": "test"}
                }
            }),
            session,
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted
        | ImmediateResult::Stream(_)
        | ImmediateResult::TaskStream { .. } => {
            panic!("initialize must return a response")
        }
    };
    assert_eq!(
        initialized["result"]["instructions"],
        "Eval debugger\n\nStart with find_runs."
    );
    assert_eq!(
        initialized["result"]["serverInfo"]["title"],
        "Eval debugger"
    );
}

#[tokio::test]
async fn tools_list_hides_host_injected_authority() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn inspect(harness: Harness, hypothesis_id: string) -> string {
  return hypothesis_id
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let tools = server.tools_list_result(&json!({}));
    let schema = &tools["tools"][0]["inputSchema"];
    assert!(schema["properties"].get("harness").is_none());
    assert_eq!(schema["required"], json!(["hypothesis_id"]));
}

#[tokio::test]
async fn discover_and_resources_expose_server_card() {
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

    let init = server.handle_server_discover(json!(1));
    assert_eq!(
        init["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["card"]["name"],
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
async fn released_initialize_lifecycle_lists_generic_server_tools() {
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

    let initialized = match server
        .process_message(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "codex-mcp-client", "version": "test"}
                }
            }),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted
        | ImmediateResult::Stream(_)
        | ImmediateResult::TaskStream { .. } => {
            panic!("initialize must return a response")
        }
    };
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-11-25")
    );
    assert_eq!(
        session.connection().client_identity(),
        "codex-mcp-client/test"
    );

    let tools = match server
        .process_message(
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
            session,
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted
        | ImmediateResult::Stream(_)
        | ImmediateResult::TaskStream { .. } => {
            panic!("tools/list must return a response")
        }
    };
    assert_eq!(tools["result"]["tools"][0]["name"], json!("greet"));
    assert!(tools["result"].get("resultType").is_none());
    assert!(tools["result"].get("ttlMs").is_none());
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
    std::fs::write(
        dir.path().join("server.md"),
        "# How to use\n\nStart with greet.\n",
    )
    .expect("write howto");
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
    let init = server.handle_server_discover(json!(1));
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
    assert!(uris.contains(&"harn://package/howto"));
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
async fn sibling_icon_is_served_on_initialize_and_as_a_resource() {
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
    std::fs::write(
        dir.path().join("server.icon.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32"/></svg>"#,
    )
    .expect("write icon");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let session = SharedSession::new();

    let initialized = match server
        .process_message(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "codex-mcp-client", "version": "test"}
                }
            }),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted
        | ImmediateResult::Stream(_)
        | ImmediateResult::TaskStream { .. } => {
            panic!("initialize must return a response")
        }
    };
    let icon = &initialized["result"]["serverInfo"]["icons"][0];
    assert_eq!(icon["mimeType"], "image/svg+xml");
    assert_eq!(icon["sizes"], json!(["any"]));
    let src = icon["src"].as_str().expect("data URI");
    assert!(
        src.starts_with("data:image/svg+xml;base64,"),
        "server icon should be a data URI so a stdio client can draw it: {src}"
    );

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
    assert!(uris.contains(&"harn://package/icon"));

    let read = mcp_response(
        &server,
        harn_vm::jsonrpc::request(3, "resources/read", json!({"uri": "harn://package/icon"})),
        session,
    )
    .await;
    assert!(read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("<svg"));
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
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single("secret"))],
        mcp_allowlist: None,
    };
    let core = DispatchCore::new(config).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let session = SharedSession::new();
    let unauthorized = mcp_response(
        &server,
        harn_vm::jsonrpc::request(2, "resources/list", json!({})),
        session.clone(),
    )
    .await;
    assert_eq!(unauthorized["error"]["code"], json!(-32001));

    let authorized = match server
        .process_message(
            stable_request(harn_vm::jsonrpc::request(3, "resources/list", json!({}))),
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
        ImmediateResult::Accepted
        | ImmediateResult::Stream(_)
        | ImmediateResult::TaskStream { .. } => {
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

async fn mcp_response(server: &McpServer, request: JsonValue, session: SharedSession) -> JsonValue {
    match server
        .process_message(stable_request(request), session, AuthRequest::default())
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Accepted
        | ImmediateResult::Stream(_)
        | ImmediateResult::TaskStream { .. } => {
            panic!("expected MCP JSON-RPC response")
        }
    }
}

async fn mcp_tool_response(
    server: &McpServer,
    request: JsonValue,
    session: SharedSession,
) -> JsonValue {
    let job = match server
        .process_message(stable_request(request), session, AuthRequest::default())
        .await
    {
        ImmediateResult::Stream(job) => job,
        ImmediateResult::Response(_)
        | ImmediateResult::Accepted
        | ImmediateResult::TaskStream { .. } => {
            panic!("expected MCP tool stream job")
        }
    };
    let responses = Arc::new(Mutex::new(Vec::new()));
    let captured = responses.clone();
    server
        .execute_streaming_job(
            *job,
            notify_channel(move |response| {
                captured.lock().expect("response lock").push(response);
            }),
        )
        .await;
    let response = responses
        .lock()
        .expect("response lock")
        .pop()
        .expect("tool response");
    response
}

fn stable_request(mut request: JsonValue) -> JsonValue {
    let params = request
        .get_mut("params")
        .and_then(JsonValue::as_object_mut)
        .expect("test request params object");
    let meta = params
        .entry("_meta")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("test request metadata object");
    meta.entry(mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION)
        .or_insert_with(|| json!(MCP_PROTOCOL_VERSION));
    meta.entry(mcp_protocol::MCP_META_KEY_CLIENT_INFO)
        .or_insert_with(|| json!({"name": "test", "version": "1"}));
    meta.entry(mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES)
        .or_insert_with(|| json!({}));
    request
}

#[tokio::test]
async fn repeated_tool_calls_with_same_arguments_observe_mutated_backing_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn observe_execution() -> int {
  return test_increment_call_count()
}
",
    )
    .expect("write script");

    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = DispatchCoreConfig::for_script(&script);
    config.vm_configurator = Arc::new(McpMutationConfigurator {
        calls: calls.clone(),
    });
    let core = DispatchCore::new(config).expect("core");
    let server = McpServer::new(McpServerConfig::new(core));
    let session = SharedSession::new();
    let first = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(
            2,
            "tools/call",
            json!({"name": "observe_execution", "arguments": {}}),
        ),
        session.clone(),
    )
    .await;
    let second = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(
            3,
            "tools/call",
            json!({"name": "observe_execution", "arguments": {}}),
        ),
        session,
    )
    .await;

    assert_eq!(
        [
            first["result"]["content"][0]["text"].clone(),
            second["result"]["content"][0]["text"].clone(),
        ],
        [json!("1"), json!("2")]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn safe_calls_complete_out_of_order_by_id_and_cancellation_stays_request_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
@annotations(readOnly: true, idempotent: true)
pub fn rendezvous(value: string) -> string {
  return test_rendezvous(value)
}
",
    )
    .expect("write script");
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let releases = Arc::new(std::sync::Mutex::new(HashMap::from([
        (
            "first".to_string(),
            Arc::new(tokio::sync::Semaphore::new(0)),
        ),
        (
            "second".to_string(),
            Arc::new(tokio::sync::Semaphore::new(0)),
        ),
        (
            "cancel".to_string(),
            Arc::new(tokio::sync::Semaphore::new(0)),
        ),
    ])));
    let mut config = DispatchCoreConfig::for_script(&script);
    config.max_dispatch_workers = NonZeroUsize::new(2).expect("two workers");
    config.vm_configurator = Arc::new(McpRendezvousConfigurator {
        entered: entered_tx,
        releases: Arc::clone(&releases),
    });
    let server = Arc::new(McpServer::new(McpServerConfig::new(
        DispatchCore::new(config).expect("core"),
    )));
    let session = SharedSession::new();

    async fn job(
        server: &McpServer,
        session: SharedSession,
        id: u64,
        value: &str,
    ) -> Box<StreamJob> {
        match server
            .process_message(
                stable_request(harn_vm::jsonrpc::request(
                    id,
                    "tools/call",
                    json!({"name": "rendezvous", "arguments": {"value": value}}),
                )),
                session,
                AuthRequest::default(),
            )
            .await
        {
            ImmediateResult::Stream(job) => job,
            _ => panic!("tools/call must produce a stream job"),
        }
    }

    let first_job = job(&server, session.clone(), 41, "first").await;
    let second_job = job(&server, session.clone(), 42, "second").await;
    let (response_tx, mut response_rx) = tokio::sync::mpsc::unbounded_channel();
    let notifier = notify_channel(move |message| {
        let _ = response_tx.send(message);
    });
    let first_server = Arc::clone(&server);
    let first_notify = Arc::clone(&notifier);
    let first = tokio::spawn(async move {
        first_server
            .execute_streaming_job(*first_job, first_notify)
            .await;
    });
    let second_server = Arc::clone(&server);
    let second_notify = Arc::clone(&notifier);
    let second = tokio::spawn(async move {
        second_server
            .execute_streaming_job(*second_job, second_notify)
            .await;
    });

    let entered_a = entered_rx.recv().await.expect("first safe call entered");
    let entered_b = entered_rx.recv().await.expect("second safe call entered");
    assert_ne!(entered_a, entered_b);
    releases
        .lock()
        .expect("release map")
        .get("second")
        .expect("second release")
        .add_permits(1);
    assert_eq!(response_rx.recv().await.expect("second response")["id"], 42);
    releases
        .lock()
        .expect("release map")
        .get("first")
        .expect("first release")
        .add_permits(1);
    assert_eq!(response_rx.recv().await.expect("first response")["id"], 41);
    first.await.expect("first execution task");
    second.await.expect("second execution task");

    let cancelled_job = job(&server, session.clone(), 43, "cancel").await;
    let cancelled_server = Arc::clone(&server);
    let cancelled_notify = Arc::clone(&notifier);
    let cancelled = tokio::spawn(async move {
        cancelled_server
            .execute_streaming_job(*cancelled_job, cancelled_notify)
            .await;
    });
    assert_eq!(entered_rx.recv().await.as_deref(), Some("cancel"));
    server.handle_cancel_notification(&session, &json!({"requestId": 43}));
    releases
        .lock()
        .expect("release map")
        .get("cancel")
        .expect("cancel release")
        .add_permits(1);
    cancelled.await.expect("cancelled execution task");
    assert!(response_rx.try_recv().is_err());
}

#[tokio::test]
async fn client_task_capability_does_not_enable_unadvertised_server_tasks() {
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
    let response = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(
            2,
            "tools/call",
            json!({
                "name": "greet",
                "arguments": {"name": "alice"},
                "_meta": {
                    harn_vm::mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES: {
                        "extensions": {mcp_protocol::TASKS_EXTENSION_ID: {}}
                    }
                }
            }),
        ),
        session,
    )
    .await;

    assert_eq!(response["result"]["content"][0]["text"], json!("alice"));
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

/// A script with one `@job` export and one ordinary export.
fn job_export_server(dir: &tempfile::TempDir) -> McpServer {
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
@job("nightly_rollup")
pub fn rollup(day: string) -> string {
  return day
}

pub type RollupError = {variant: "Unavailable", secret: string}

@job("failing_rollup")
pub fn fail_rollup() -> string throws RollupError {
  throw {variant: "Unavailable", secret: "typed-detail"}
}

pub fn greet(name: string) -> string {
  return name
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    McpServer::new(McpServerConfig::new(core))
}

/// A `_meta` block for a client that carries the tasks extension.
fn task_client_request(request: JsonValue) -> JsonValue {
    let mut request = stable_request(request);
    request["params"]["_meta"][mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES] = json!({
        "extensions": {mcp_protocol::TASKS_EXTENSION_ID: {}}
    });
    request
}

/// `@job` remains Harn's server-side scheduling decision. The stable MCP tasks
/// extension deliberately removed tool-level taskSupport discovery.
#[tokio::test]
async fn tools_list_omits_retired_tool_level_task_support() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = job_export_server(&dir).tools_list_result(&json!({}));
    let by_name: std::collections::BTreeMap<&str, &JsonValue> = tools["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| (tool["name"].as_str().expect("tool name"), tool))
        .collect();
    assert!(by_name["rollup"].get("execution").is_none());
    assert!(by_name["greet"].get("execution").is_none());
}

/// The capability is advertised only when something can actually be served that
/// way. A server whose script has no `@job` export has nothing to run as a
/// task, and saying otherwise would leave a client unable to tell "no tasks
/// here" from "your task is gone".
#[tokio::test]
async fn tasks_capability_tracks_whether_any_export_declares_a_job() {
    let with_job = tempfile::tempdir().expect("tempdir");
    let discover = job_export_server(&with_job).handle_server_discover(json!(1));
    assert!(
        discover["result"]["capabilities"]["extensions"][mcp_protocol::TASKS_EXTENSION_ID]
            .is_object()
    );

    let without = tempfile::tempdir().expect("tempdir");
    let script = without.path().join("server.harn");
    std::fs::write(
        &script,
        "pub fn greet(name: string) -> string {\n  return name\n}\n",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let plain = McpServer::new(McpServerConfig::new(core)).handle_server_discover(json!(1));
    assert!(
        plain["result"]["capabilities"].get("extensions").is_none(),
        "a server with no job export must not advertise tasks: {plain}",
    );
}

/// The whole round trip: ask for a task, get an id back instead of a result,
/// and collect the result from `tasks/get` once the job lands.
#[tokio::test]
async fn a_job_export_runs_as_a_task_the_client_can_collect() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = Arc::new(job_export_server(&dir));
    let session = SharedSession::new();

    let (immediate, job) = match server
        .process_message(
            task_client_request(harn_vm::jsonrpc::request(
                1,
                "tools/call",
                json!({"name": "rollup", "arguments": {"day": "2026-08-23"}}),
            )),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::TaskStream { immediate, job } => (immediate, job),
        other => panic!(
            "a task-capable client calling a job export must get a task, got {:?}",
            matches!(other, ImmediateResult::Stream(_))
        ),
    };
    assert_eq!(immediate["result"]["resultType"], json!("task"));
    assert_eq!(immediate["result"]["status"], json!("working"));
    let task_id = immediate["result"]["taskId"]
        .as_str()
        .expect("a created task carries an id")
        .to_string();

    server
        .execute_streaming_job(*job, notify_channel(|_| {}))
        .await;

    let read = mcp_response(
        &server,
        task_client_request(harn_vm::jsonrpc::request(
            2,
            "tasks/get",
            json!({"taskId": task_id}),
        )),
        session,
    )
    .await;
    assert_eq!(read["result"]["status"], json!("completed"));
    assert_eq!(read["result"]["result"]["isError"], json!(false));
    assert_eq!(
        read["result"]["result"]["content"][0]["text"],
        json!("2026-08-23"),
    );
}

#[tokio::test]
async fn declared_application_error_completes_task_with_typed_error_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = Arc::new(job_export_server(&dir));
    let session = SharedSession::new();
    let (immediate, job) = match server
        .process_message(
            task_client_request(harn_vm::jsonrpc::request(
                1,
                "tools/call",
                json!({"name": "fail_rollup", "arguments": {}}),
            )),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::TaskStream { immediate, job } => (immediate, job),
        _ => panic!("task-capable declared error must create a task"),
    };
    let task_id = immediate["result"]["taskId"]
        .as_str()
        .expect("task id")
        .to_string();
    server
        .execute_streaming_job(*job, notify_channel(|_| {}))
        .await;

    let read = mcp_response(
        &server,
        task_client_request(harn_vm::jsonrpc::request(
            2,
            "tasks/get",
            json!({"taskId": task_id}),
        )),
        session,
    )
    .await;
    assert_eq!(read["result"]["status"], "completed");
    assert_eq!(read["result"]["result"]["isError"], true);
    assert_eq!(
        read["result"]["result"]["_meta"][harn_vm::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
            ["applicationError"]["data"]["variant"],
        "Unavailable"
    );
    assert!(read["result"]["result"].get("structuredContent").is_none());
}

/// Opting in is per client as well as per export: a client that never declared
/// the extension keeps getting its result on the wire.
#[tokio::test]
async fn a_client_without_the_extension_still_calls_a_job_export_inline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = job_export_server(&dir);
    let response = mcp_tool_response(
        &server,
        harn_vm::jsonrpc::request(
            1,
            "tools/call",
            json!({"name": "rollup", "arguments": {"day": "2026-08-23"}}),
        ),
        SharedSession::new(),
    )
    .await;
    assert!(
        response["result"].get("taskId").is_none(),
        "an opted-out client must not be handed a task: {response}",
    );
    assert_eq!(
        response["result"]["content"][0]["text"],
        json!("2026-08-23")
    );
}
