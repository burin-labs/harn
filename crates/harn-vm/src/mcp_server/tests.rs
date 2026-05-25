use std::collections::BTreeMap;
use std::rc::Rc;

use crate::chunk::{Chunk, CompiledFunction};
use crate::value::{VmClosure, VmEnv, VmValue};

use super::convert::{annotations_to_json, prompt_value_to_messages};
use super::defs::{
    McpCompletionSource, McpPromptArgDef, McpPromptDef, McpResourceDef, McpResourceTemplateDef,
};
use super::tool_registry_to_mcp_tools;
use super::tools_schema::params_to_json_schema;
use super::uri::match_uri_template;
use super::McpServer;

fn empty_closure(name: &str) -> VmClosure {
    VmClosure {
        func: Rc::new(CompiledFunction {
            name: name.to_string(),
            type_params: Vec::new(),
            nominal_type_names: Vec::new(),
            params: Vec::new(),
            default_start: None,
            chunk: Rc::new(Chunk::new()),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks: false,
        }),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
    }
}

#[test]
fn test_params_to_json_schema_empty() {
    let schema = params_to_json_schema(None);
    assert_eq!(
        schema,
        serde_json::json!({ "type": "object", "properties": {} })
    );
}

#[test]
fn test_params_to_json_schema_with_params() {
    let mut params = BTreeMap::new();
    let mut param_def = BTreeMap::new();
    param_def.insert("type".to_string(), VmValue::String(Rc::from("string")));
    param_def.insert(
        "description".to_string(),
        VmValue::String(Rc::from("A file path")),
    );
    param_def.insert("required".to_string(), VmValue::Bool(true));
    params.insert("path".to_string(), VmValue::Dict(Rc::new(param_def)));

    let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
    assert_eq!(
        schema,
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "A file path" } },
            "required": ["path"]
        })
    );
}

#[test]
fn test_params_to_json_schema_preserves_file_input_descriptor() {
    let mut file_descriptor = BTreeMap::new();
    file_descriptor.insert(
        "accept".to_string(),
        VmValue::List(Rc::new(vec![VmValue::String(Rc::from("text/*"))])),
    );
    file_descriptor.insert("maxSize".to_string(), VmValue::Int(64));

    let mut param_def = BTreeMap::new();
    param_def.insert("type".to_string(), VmValue::String(Rc::from("string")));
    param_def.insert("format".to_string(), VmValue::String(Rc::from("uri")));
    param_def.insert(
        "x-mcp-file".to_string(),
        VmValue::Dict(Rc::new(file_descriptor)),
    );
    param_def.insert("required".to_string(), VmValue::Bool(true));

    let mut params = BTreeMap::new();
    params.insert("upload".to_string(), VmValue::Dict(Rc::new(param_def)));

    let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
    assert_eq!(schema["properties"]["upload"]["type"], "string");
    assert_eq!(schema["properties"]["upload"]["format"], "uri");
    assert_eq!(
        schema["properties"]["upload"]["x-mcp-file"]["accept"],
        serde_json::json!(["text/*"])
    );
    assert_eq!(schema["properties"]["upload"]["x-mcp-file"]["maxSize"], 64);
    assert_eq!(schema["required"], serde_json::json!(["upload"]));
}

#[test]
fn test_params_to_json_schema_simple_form() {
    let mut params = BTreeMap::new();
    params.insert("query".to_string(), VmValue::String(Rc::from("string")));
    let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
    assert_eq!(
        schema["properties"]["query"]["type"],
        serde_json::json!("string")
    );
}

#[test]
fn test_tool_registry_to_mcp_tools_invalid() {
    assert!(tool_registry_to_mcp_tools(&VmValue::Nil).is_err());
}

#[test]
fn test_tool_registry_to_mcp_tools_empty() {
    let mut registry = BTreeMap::new();
    registry.insert("_type".into(), VmValue::String(Rc::from("tool_registry")));
    registry.insert("tools".into(), VmValue::List(Rc::new(Vec::new())));
    let result = tool_registry_to_mcp_tools(&VmValue::Dict(Rc::new(registry)));
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_tool_registry_to_mcp_tools_preserves_metadata() {
    let handler = VmValue::Closure(Rc::new(empty_closure("echo")));

    let mut annotations = BTreeMap::new();
    annotations.insert("readOnlyHint".into(), VmValue::Bool(true));
    annotations.insert("idempotentHint".into(), VmValue::Bool(true));

    let icon = VmValue::Dict(Rc::new({
        let mut icon = BTreeMap::new();
        icon.insert(
            "src".into(),
            VmValue::String(Rc::from("https://example.com/tool.png")),
        );
        icon.insert("mimeType".into(), VmValue::String(Rc::from("image/png")));
        icon
    }));

    let mut tool = BTreeMap::new();
    tool.insert("name".into(), VmValue::String(Rc::from("echo")));
    tool.insert("title".into(), VmValue::String(Rc::from("Echo")));
    tool.insert(
        "description".into(),
        VmValue::String(Rc::from("Echo input")),
    );
    tool.insert("handler".into(), handler);
    tool.insert("parameters".into(), VmValue::Dict(Rc::new(BTreeMap::new())));
    tool.insert("annotations".into(), VmValue::Dict(Rc::new(annotations)));
    tool.insert("icons".into(), VmValue::List(Rc::new(vec![icon])));
    tool.insert(
        "outputSchema".into(),
        VmValue::Dict(Rc::new({
            let mut schema = BTreeMap::new();
            schema.insert("type".into(), VmValue::String(Rc::from("string")));
            schema
        })),
    );

    let mut registry = BTreeMap::new();
    registry.insert("_type".into(), VmValue::String(Rc::from("tool_registry")));
    registry.insert(
        "tools".into(),
        VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(tool))])),
    );

    let tools = tool_registry_to_mcp_tools(&VmValue::Dict(Rc::new(registry))).unwrap();
    assert_eq!(tools[0].title.as_deref(), Some("Echo"));
    assert_eq!(tools[0].annotations.as_ref().unwrap()["readOnlyHint"], true);
    assert_eq!(
        tools[0].icons.as_ref().unwrap()[0]["src"],
        "https://example.com/tool.png"
    );
    assert_eq!(tools[0].output_schema.as_ref().unwrap()["type"], "string");
}

#[test]
fn test_prompt_value_to_messages_string() {
    let msgs = prompt_value_to_messages(&VmValue::String(Rc::from("hello")));
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"]["text"], "hello");
}

#[test]
fn test_prompt_value_to_messages_list() {
    let items = vec![
        VmValue::Dict(Rc::new({
            let mut d = BTreeMap::new();
            d.insert("role".into(), VmValue::String(Rc::from("user")));
            d.insert("content".into(), VmValue::String(Rc::from("hi")));
            d
        })),
        VmValue::Dict(Rc::new({
            let mut d = BTreeMap::new();
            d.insert("role".into(), VmValue::String(Rc::from("assistant")));
            d.insert("content".into(), VmValue::String(Rc::from("hello")));
            d
        })),
    ];
    let msgs = prompt_value_to_messages(&VmValue::List(Rc::new(items)));
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1]["role"], "assistant");
}

#[test]
fn test_prompt_value_to_messages_preserves_image_content() {
    let items = vec![VmValue::Dict(Rc::new({
        let mut image = BTreeMap::new();
        image.insert("type".into(), VmValue::String(Rc::from("image")));
        image.insert("data".into(), VmValue::String(Rc::from("ZmFrZQ==")));
        image.insert("mimeType".into(), VmValue::String(Rc::from("image/png")));

        let mut message = BTreeMap::new();
        message.insert("role".into(), VmValue::String(Rc::from("user")));
        message.insert("content".into(), VmValue::Dict(Rc::new(image)));
        message
    }))];
    let msgs = prompt_value_to_messages(&VmValue::List(Rc::new(items)));
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"]["type"], "image");
    assert_eq!(msgs[0]["content"]["data"], "ZmFrZQ==");
    assert_eq!(msgs[0]["content"]["mimeType"], "image/png");
}

#[test]
fn test_match_uri_template_simple() {
    let vars = match_uri_template("file:///{path}", "file:///foo/bar.rs").unwrap();
    assert_eq!(vars["path"], "foo/bar.rs");
}

#[test]
fn test_match_uri_template_multiple() {
    let vars = match_uri_template("db://{schema}/{table}", "db://public/users").unwrap();
    assert_eq!(vars["schema"], "public");
    assert_eq!(vars["table"], "users");
}

#[test]
fn test_match_uri_template_no_match() {
    assert!(match_uri_template("file:///{path}", "http://example.com").is_none());
}

#[test]
fn test_annotations_to_json() {
    let mut d = BTreeMap::new();
    d.insert("title".into(), VmValue::String(Rc::from("My Tool")));
    d.insert("readOnlyHint".into(), VmValue::Bool(true));
    d.insert("destructiveHint".into(), VmValue::Bool(false));
    let json = annotations_to_json(&VmValue::Dict(Rc::new(d))).unwrap();
    assert_eq!(json["title"], "My Tool");
    assert_eq!(json["readOnlyHint"], true);
    assert_eq!(json["destructiveHint"], false);
}

#[test]
fn test_annotations_empty_returns_none() {
    let d = BTreeMap::new();
    assert!(annotations_to_json(&VmValue::Dict(Rc::new(d))).is_none());
}

#[tokio::test]
async fn server_advertises_resource_list_changed_capability() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            text: "hello".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "initialize",
                serde_json::json!({"protocolVersion": super::PROTOCOL_VERSION}),
            ),
            &mut vm,
        )
        .await
        .expect("response");

    assert_eq!(
        response["result"]["capabilities"]["resources"]["listChanged"],
        serde_json::json!(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["resources"]["subscribe"],
        serde_json::json!(true)
    );
    assert_eq!(
        response["result"]["capabilities"]["tasks"]["requests"]["tools"]["call"],
        serde_json::json!({})
    );
}

#[tokio::test]
async fn server_accepts_resource_subscribe_and_unsubscribe_for_registered_uris() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            text: "hello".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let subscribed = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "resources/subscribe",
                serde_json::json!({ "uri": "docs://readme" }),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(subscribed["result"], serde_json::json!({}));

    let unsubscribed = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                "resources/unsubscribe",
                serde_json::json!({ "uri": "docs://readme" }),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(unsubscribed["result"], serde_json::json!({}));
}

#[tokio::test]
async fn server_advertises_elicitation_capability() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "initialize",
                serde_json::json!({"protocolVersion": super::PROTOCOL_VERSION}),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    // Capability is signaled by an empty object — its presence
    // alone tells the client elicitation is available.
    assert!(
        response["result"]["capabilities"]["elicitation"].is_object(),
        "expected elicitation capability, got {response:?}"
    );
}

#[tokio::test]
async fn server_completion_complete_returns_prompt_and_resource_suggestions() {
    let mut resource_completions = BTreeMap::new();
    resource_completions.insert(
        "key".to_string(),
        McpCompletionSource {
            values: vec!["name".to_string(), "version".to_string()],
            handler: None,
        },
    );
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        vec![McpResourceTemplateDef {
            uri_template: "config://{key}".to_string(),
            name: "Configuration".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            completions: resource_completions,
            handler: empty_closure("resource"),
        }],
        vec![McpPromptDef {
            name: "review".to_string(),
            title: None,
            description: None,
            arguments: Some(vec![McpPromptArgDef {
                name: "language".to_string(),
                description: None,
                required: false,
                completion: Some(McpCompletionSource {
                    values: vec![
                        "rust".to_string(),
                        "ruby".to_string(),
                        "typescript".to_string(),
                    ],
                    handler: None,
                }),
            }]),
            handler: empty_closure("prompt"),
        }],
    );
    let mut vm = crate::Vm::new();

    let init = server
        .handle_json_rpc(
            crate::jsonrpc::request(1, "initialize", serde_json::json!({})),
            &mut vm,
        )
        .await
        .expect("response");
    assert!(init["result"]["capabilities"]["completions"].is_object());

    let prompt = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                crate::mcp_protocol::METHOD_COMPLETION_COMPLETE,
                serde_json::json!({
                    "ref": {"type": "ref/prompt", "name": "review"},
                    "argument": {"name": "language", "value": "ru"},
                }),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        prompt["result"]["completion"]["values"],
        serde_json::json!(["ruby", "rust"])
    );
    assert_eq!(
        prompt["result"]["completion"]["total"],
        serde_json::json!(2)
    );
    assert_eq!(
        prompt["result"]["completion"]["hasMore"],
        serde_json::json!(false)
    );

    let resource = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                3,
                crate::mcp_protocol::METHOD_COMPLETION_COMPLETE,
                serde_json::json!({
                    "ref": {"type": "ref/resource", "uri": "config://{key}"},
                    "argument": {"name": "key", "value": "ver"},
                }),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        resource["result"]["completion"]["values"],
        serde_json::json!(["version"])
    );
}

#[tokio::test]
async fn server_rejects_client_bound_sampling_and_elicitation_requests() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    for (method, feature) in [
        (
            crate::mcp_protocol::METHOD_SAMPLING_CREATE_MESSAGE,
            "sampling",
        ),
        (
            crate::mcp_protocol::METHOD_ELICITATION_CREATE,
            "elicitation",
        ),
    ] {
        let response = server
            .handle_json_rpc(
                crate::jsonrpc::request(7, method, serde_json::json!({})),
                &mut vm,
            )
            .await
            .expect("response");
        assert!(response.get("result").is_none());
        assert_eq!(response["error"]["code"], serde_json::json!(-32601));
        assert_eq!(response["error"]["data"]["feature"], feature);
        assert_eq!(response["error"]["data"]["role"], "client");
    }
}

#[tokio::test]
async fn server_tool_call_rejects_task_augmentation() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/call",
                serde_json::json!({
                    "name": "missing",
                    "arguments": {},
                    "task": {"title": "async please"}
                }),
            ),
            &mut vm,
        )
        .await
        .expect("response");

    assert_eq!(response["error"]["code"], serde_json::json!(-32602));
    assert_eq!(
        response["error"]["data"]["feature"],
        serde_json::json!("tasks")
    );
}

fn rc_metadata_params(rest: serde_json::Value) -> serde_json::Value {
    let mut object = match rest {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        other => serde_json::Map::from_iter([("value".to_string(), other)]),
    };
    object.insert(
        "_meta".to_string(),
        serde_json::json!({
            crate::mcp_protocol::RC_META_KEY_PROTOCOL_VERSION:
                crate::mcp_protocol::DRAFT_PROTOCOL_VERSION,
            crate::mcp_protocol::RC_META_KEY_CLIENT_INFO: {"name": "rc-client", "version": "1.0"},
            crate::mcp_protocol::RC_META_KEY_CLIENT_CAPABILITIES: {},
        }),
    );
    serde_json::Value::Object(object)
}

#[tokio::test]
async fn server_discover_returns_capabilities_and_supported_versions() {
    let server = McpServer::new(
        "rc-test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            text: "hello".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                crate::mcp_protocol::METHOD_SERVER_DISCOVER,
                serde_json::json!({}),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        response["result"]["resultType"],
        serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(
        response["result"]["protocolVersion"],
        serde_json::json!(crate::mcp_protocol::DRAFT_PROTOCOL_VERSION)
    );
    let supported = response["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions");
    assert!(supported
        .iter()
        .any(|v| v == &serde_json::json!(crate::mcp_protocol::DRAFT_PROTOCOL_VERSION)));
    assert!(supported
        .iter()
        .any(|v| v == &serde_json::json!(crate::mcp_protocol::PROTOCOL_VERSION)));
    assert_eq!(
        response["result"]["capabilities"]["resources"]["subscribe"],
        serde_json::json!(true)
    );
    assert_eq!(response["result"]["serverInfo"]["name"], "rc-test");
}

#[tokio::test]
async fn server_rejects_request_with_unsupported_protocol_version_metadata() {
    let server = McpServer::new(
        "rc-test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                7,
                "tools/list",
                serde_json::json!({
                    "_meta": {
                        crate::mcp_protocol::RC_META_KEY_PROTOCOL_VERSION: "2099-01-01"
                    }
                }),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        response["error"]["code"],
        serde_json::json!(crate::mcp_protocol::UNSUPPORTED_PROTOCOL_VERSION_CODE)
    );
    let supported = response["error"]["data"]["supported"]
        .as_array()
        .expect("supported array");
    assert!(supported
        .iter()
        .any(|v| v == &serde_json::json!(crate::mcp_protocol::DRAFT_PROTOCOL_VERSION)));
}

#[tokio::test]
async fn server_modern_tools_list_emits_result_type_and_cache_hint() {
    let server = McpServer::new(
        "rc-test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            text: "hello".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let modern = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "resources/list",
                rc_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("modern response");
    assert_eq!(
        modern["result"]["resultType"],
        serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(
        modern["result"]["cacheScope"],
        serde_json::json!(crate::mcp_protocol::DEFAULT_LIST_CACHE_SCOPE)
    );
    assert_eq!(
        modern["result"]["ttlMs"],
        serde_json::json!(crate::mcp_protocol::DEFAULT_LIST_CACHE_TTL_MS)
    );

    let legacy = server
        .handle_json_rpc(
            crate::jsonrpc::request(2, "resources/list", serde_json::json!({})),
            &mut vm,
        )
        .await
        .expect("legacy response");
    assert!(
        legacy["result"].get("resultType").is_none(),
        "legacy clients must not see resultType: {legacy}"
    );
    assert!(legacy["result"].get("ttlMs").is_none());
    assert!(legacy["result"].get("cacheScope").is_none());
}

#[tokio::test]
async fn server_modern_non_list_methods_carry_result_type_envelope() {
    let server = McpServer::new(
        "rc-test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    for method in [
        "ping",
        "logging/setLevel",
        crate::mcp_protocol::METHOD_TASKS_LIST,
    ] {
        let params = rc_metadata_params(serde_json::json!({"level": "warning"}));
        let response = server
            .handle_json_rpc(crate::jsonrpc::request(1, method, params), &mut vm)
            .await
            .expect("response");
        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("{method} should return result, got {response:?}"));
        assert_eq!(
            result["resultType"],
            serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE),
            "{method} modern response must carry RC envelope"
        );
    }
}

#[tokio::test]
async fn server_initialize_echoes_client_requested_protocol_version() {
    let server = McpServer::new(
        "rc-test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "initialize",
                serde_json::json!({"protocolVersion": "2025-11-25"}),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");

    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                "initialize",
                serde_json::json!({"protocolVersion": "2099-12-31"}),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        response["result"]["protocolVersion"],
        crate::mcp_protocol::PROTOCOL_VERSION,
        "unsupported version must negotiate down to the stable default"
    );
}

#[tokio::test]
async fn server_modern_resources_read_emits_cache_hint() {
    let server = McpServer::new(
        "rc-test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            text: "hello".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "resources/read",
                rc_metadata_params(serde_json::json!({"uri": "docs://readme"})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        response["result"]["resultType"],
        serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(
        response["result"]["ttlMs"],
        serde_json::json!(crate::mcp_protocol::DEFAULT_READ_CACHE_TTL_MS)
    );
}

#[tokio::test]
async fn server_task_endpoints_are_protocol_shaped_without_inline_tasks() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let listed = server
        .handle_json_rpc(
            crate::jsonrpc::request(1, "tasks/list", serde_json::json!({})),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(listed["result"]["tasks"], serde_json::json!([]));

    let missing = server
        .handle_json_rpc(
            crate::jsonrpc::request(2, "tasks/get", serde_json::json!({"taskId": "missing"})),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(missing["error"]["code"], serde_json::json!(-32602));
}
