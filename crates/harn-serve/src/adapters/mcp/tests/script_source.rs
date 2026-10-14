//! The script-published registry served through the one MCP server: the same
//! sessions, authorization, and task ownership as exported functions.
use super::*;

const SCRIPT: &str = r#"
import { tool_registry_from } from "std/tools"

fn main(harness: Harness) {
  harness.tools.mcp_server_metadata({name: "script-fixture", instructions: "Script fixture"})
  const tools = tool_registry_from([
    {
      name: "greet",
      description: "Greet one person.",
      parameters: {name: {schema: {type: "string"}, required: true}},
      execution: {taskSupport: "optional"},
      handler: {args -> "Hello, " + args.name},
    },
    {
      name: "count",
      description: "Report progress, then finish.",
      parameters: {},
      handler: {_args ->
        harness.tools.mcp_report_progress(1, {total: 2, message: "halfway"})
        "counted"
      },
    },
    {
      name: "audit",
      description: "Only runs as a task.",
      parameters: {},
      execution: {taskSupport: "required"},
      handler: {_args -> "audited"},
    },
  ], {info: {name: "script-fixture"}})
  harness.tools.mcp_tools(tools)
  harness.tools.mcp_resource({uri: "docs://readme", name: "README", mime_type: "text/plain", text: "readme text"})
  harness.tools.mcp_prompt({
    name: "review",
    arguments: [{name: "code", required: true}],
    handler: {args -> "Review: " + args.code},
  })
}
"#;

/// Run `SCRIPT` the way `harn serve mcp` does and collect its publication.
async fn load_registry() -> (harn_vm::McpScriptRegistry, harn_vm::Vm) {
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    harn_vm::reset_mcp_serve_publication();
    let chunk = harn_vm::compile_source(SCRIPT).expect("compile fixture");
    vm.execute(&chunk).await.expect("run fixture");
    let published = harn_vm::take_mcp_serve_registry().expect("fixture publishes tools");
    let tools = harn_vm::tool_registry_to_mcp_tools(&published).expect("valid tools");
    let mut registry = harn_vm::McpScriptRegistry::new(
        "fixture".to_string(),
        tools,
        harn_vm::take_mcp_serve_resources(),
        harn_vm::take_mcp_serve_resource_templates(),
        harn_vm::take_mcp_serve_prompts(),
    );
    if let Some(metadata) = harn_vm::take_mcp_serve_metadata() {
        registry = registry.with_metadata(metadata);
    }
    (registry, vm)
}

async fn script_server(auth_policy: crate::AuthPolicy) -> Arc<McpServer> {
    let (registry, vm) = load_registry().await;
    let runtime = McpScriptRuntime::spawn_local(registry, vm, ());
    Arc::new(McpServer::new(
        McpServerConfig::script(runtime).with_auth_policy(auth_policy),
    ))
}

fn two_key_policy() -> crate::AuthPolicy {
    crate::AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: vec![
                crate::ApiKeyEntry::new("key-a", []),
                crate::ApiKeyEntry::new("key-b", []),
            ],
        })],
        mcp_allowlist: None,
    }
}

async fn respond(server: &McpServer, request: JsonValue, auth: AuthRequest) -> JsonValue {
    match server
        .process_message(request, SharedSession::new(), auth)
        .await
    {
        ImmediateResult::Response(response) => response,
        ImmediateResult::Stream(job) => server
            .run_job_to_response(*job)
            .await
            .expect("tool response"),
        ImmediateResult::Accepted | ImmediateResult::TaskStream { .. } => {
            panic!("expected one response")
        }
    }
}

fn local<F: std::future::Future<Output = ()>>(test: F) -> impl std::future::Future<Output = ()> {
    let set = tokio::task::LocalSet::new();
    async move { set.run_until(test).await }
}

#[tokio::test]
async fn script_registry_serves_tools_resources_and_prompts() {
    local(async {
        let server = script_server(crate::AuthPolicy::default()).await;
        let discover = server.handle_server_discover(json!(1));
        let capabilities = &discover["result"]["capabilities"];
        for family in ["tools", "resources", "prompts", "completions"] {
            assert!(capabilities[family].is_object(), "{family}: {discover}");
        }
        assert!(capabilities["extensions"][mcp_protocol::TASKS_EXTENSION_ID].is_object());
        assert_eq!(
            discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "script-fixture"
        );

        let none = AuthRequest::default;
        let listed = respond(
            &server,
            stable_request(harn_vm::jsonrpc::request(2, "tools/list", json!({}))),
            none(),
        )
        .await;
        assert_eq!(listed["result"]["tools"][0]["name"], "greet");
        assert_eq!(listed["result"]["resultType"], "complete");

        let called = respond(
            &server,
            stable_request(harn_vm::jsonrpc::request(
                3,
                "tools/call",
                json!({"name": "greet", "arguments": {"name": "Ada"}}),
            )),
            none(),
        )
        .await;
        assert_eq!(called["result"]["content"][0]["text"], "Hello, Ada");
        assert_eq!(called["result"]["resultType"], "complete");

        let read = respond(
            &server,
            stable_request(harn_vm::jsonrpc::request(
                4,
                "resources/read",
                json!({"uri": "docs://readme"}),
            )),
            none(),
        )
        .await;
        assert_eq!(read["result"]["contents"][0]["text"], "readme text");
        assert_eq!(
            read["result"]["ttlMs"],
            json!(mcp_protocol::DEFAULT_READ_CACHE_TTL_MS)
        );

        let prompt = respond(
            &server,
            stable_request(harn_vm::jsonrpc::request(
                5,
                "prompts/get",
                json!({"name": "review", "arguments": {"code": "x"}}),
            )),
            none(),
        )
        .await;
        assert_eq!(
            prompt["result"]["messages"][0]["content"]["text"],
            "Review: x"
        );
    })
    .await;
}

#[tokio::test]
async fn script_handlers_report_progress_on_the_call_stream() {
    local(async {
        let server = script_server(crate::AuthPolicy::default()).await;
        let mut request = stable_request(harn_vm::jsonrpc::request(
            1,
            "tools/call",
            json!({"name": "count", "arguments": {}}),
        ));
        request["params"]["_meta"]["progressToken"] = json!("tok-1");
        let job = match server
            .process_message(request, SharedSession::new(), AuthRequest::default())
            .await
        {
            ImmediateResult::Stream(job) => job,
            _ => panic!("tools/call streams"),
        };
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = messages.clone();
        server
            .execute_streaming_job(
                *job,
                notify_channel(move |message| sink.lock().expect("sink").push(message)),
            )
            .await;
        let messages = messages.lock().expect("sink").clone();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert_eq!(messages[0]["method"], "notifications/progress");
        assert_eq!(messages[0]["params"]["progressToken"], "tok-1");
        assert_eq!(messages[0]["params"]["message"], "halfway");
        assert_eq!(messages[1]["result"]["content"][0]["text"], "counted");
    })
    .await;
}

/// Before the script registry was served here, its tasks were unscoped: any
/// authenticated caller could read or cancel another caller's task.
#[tokio::test]
async fn script_task_reads_are_bound_to_the_authenticated_principal() {
    local(async {
        let server = script_server(two_key_policy()).await;
        let (immediate, job) = match server
            .process_message(
                task_client_request(harn_vm::jsonrpc::request(
                    1,
                    "tools/call",
                    json!({"name": "greet", "arguments": {"name": "Alpha"}}),
                )),
                SharedSession::new(),
                bearer_auth("key-a"),
            )
            .await
        {
            ImmediateResult::TaskStream { immediate, job } => (immediate, job),
            _ => panic!("a task-capable client must get a task"),
        };
        let task_id = immediate["result"]["taskId"]
            .as_str()
            .expect("task id")
            .to_string();
        server.execute_streaming_job(*job, discard()).await;

        let get = |id: i64| {
            task_client_request(harn_vm::jsonrpc::request(
                id,
                "tasks/get",
                json!({"taskId": task_id}),
            ))
        };
        let as_other = respond(&server, get(2), bearer_auth("key-b")).await;
        assert_eq!(
            as_other["error"]["message"],
            "Failed to retrieve task: task not found"
        );
        let cancel_as_other = respond(
            &server,
            task_client_request(harn_vm::jsonrpc::request(
                3,
                "tasks/cancel",
                json!({"taskId": task_id}),
            )),
            bearer_auth("key-b"),
        )
        .await;
        assert!(cancel_as_other.get("error").is_some(), "{cancel_as_other}");
        let unauthenticated = respond(&server, get(4), AuthRequest::default()).await;
        assert_eq!(unauthenticated["error"]["code"], json!(-32001));

        let as_owner = respond(&server, get(5), bearer_auth("key-a")).await;
        assert_eq!(as_owner["result"]["status"], "completed");
        assert_eq!(
            as_owner["result"]["result"]["content"][0]["text"],
            "Hello, Alpha"
        );
    })
    .await;
}

#[tokio::test]
async fn script_task_support_is_enforced_before_a_task_exists() {
    local(async {
        let server = script_server(crate::AuthPolicy::default()).await;
        let plain = respond(
            &server,
            stable_request(harn_vm::jsonrpc::request(
                1,
                "tools/call",
                json!({"name": "audit", "arguments": {}}),
            )),
            AuthRequest::default(),
        )
        .await;
        assert_eq!(plain["error"]["code"], json!(-32021));

        // Invalid arguments are refused at admission, so no task is handed out.
        let invalid = match server
            .process_message(
                task_client_request(harn_vm::jsonrpc::request(
                    2,
                    "tools/call",
                    json!({"name": "greet", "arguments": {}}),
                )),
                SharedSession::new(),
                AuthRequest::default(),
            )
            .await
        {
            ImmediateResult::Response(response) => response,
            _ => panic!("a refused call must not start a task"),
        };
        assert_eq!(invalid["error"]["code"], json!(-32602));
    })
    .await;
}

#[tokio::test]
async fn hitl_respond_is_served_and_requires_authorization() {
    local(async {
        let request = || {
            stable_request(harn_vm::jsonrpc::request(
                1,
                "harn.hitl.respond",
                json!({"bogus": true}),
            ))
        };
        let open = script_server(crate::AuthPolicy::default()).await;
        let malformed = respond(&open, request(), AuthRequest::default()).await;
        assert_eq!(malformed["error"]["code"], json!(-32602));
        assert!(malformed["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid harn.hitl.respond params"));

        let guarded = script_server(two_key_policy()).await;
        let refused = respond(&guarded, request(), AuthRequest::default()).await;
        assert_eq!(refused["error"]["code"], json!(-32001));
    })
    .await;
}

#[tokio::test]
async fn reloadable_script_registry_advertises_list_changes() {
    local(async {
        let (registry, vm) = load_registry().await;
        let (runtime, _reloader) = McpScriptRuntime::spawn_local_reloadable(registry, vm, ());
        let server = McpServer::new(McpServerConfig::script(runtime));
        let initialized = match server
            .process_message(
                harn_vm::jsonrpc::request(
                    1,
                    "initialize",
                    json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "reload-client", "version": "1"},
                    }),
                ),
                SharedSession::new(),
                AuthRequest::default(),
            )
            .await
        {
            ImmediateResult::Response(response) => response,
            _ => panic!("initialize answers immediately"),
        };
        for family in ["tools", "resources", "prompts"] {
            assert_eq!(
                initialized["result"]["capabilities"][family]["listChanged"], true,
                "{family}"
            );
        }
    })
    .await;
}

#[test]
fn only_liveness_and_discovery_skip_authorization() {
    assert!(!requires_protocol_auth("server/discover"));
    assert!(!requires_protocol_auth("ping"));
    for method in [
        "tools/list",
        "tools/call",
        "resources/list",
        "resources/read",
        "resources/templates/list",
        "prompts/list",
        "prompts/get",
        "completion/complete",
        "tasks/get",
        "tasks/update",
        "tasks/cancel",
        "harn.hitl.respond",
        "sampling/createMessage",
        "some/custom/method",
    ] {
        assert!(requires_protocol_auth(method), "{method}");
    }
}
