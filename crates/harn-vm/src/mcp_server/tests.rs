use std::collections::BTreeMap;
use std::rc::Rc;

use crate::chunk::{Chunk, CompiledFunction};
use crate::value::{VmClosure, VmEnv, VmValue};

use super::convert::{annotations_to_json, prompt_value_to_messages};
use super::defs::McpResourceDef;
use super::tool_registry_to_mcp_tools;
use super::tools_schema::params_to_json_schema;
use super::uri::match_uri_template;
use super::McpServer;

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
    let handler = VmValue::Closure(Rc::new(VmClosure {
        func: Rc::new(CompiledFunction {
            name: "echo".to_string(),
            params: Vec::new(),
            default_start: None,
            chunk: Rc::new(Chunk::new()),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
        }),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
    }));

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
async fn server_latest_spec_gap_methods_return_explicit_json_rpc_errors() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    for method in crate::mcp_protocol::UNSUPPORTED_LATEST_SPEC_METHODS
        .iter()
        .map(|entry| entry.method)
    {
        let response = server
            .handle_json_rpc(
                crate::jsonrpc::request(1, method, serde_json::json!({})),
                &mut vm,
            )
            .await
            .expect("response");
        assert_eq!(response["error"]["code"], serde_json::json!(-32601));
        assert_eq!(
            response["error"]["data"]["method"],
            serde_json::json!(method)
        );
        assert_eq!(
            response["error"]["data"]["status"],
            serde_json::json!("unsupported")
        );
    }
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
        response["result"]["capabilities"]["tasks"]["requests"]["tools"]["call"],
        serde_json::json!({})
    );
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
async fn server_no_longer_auto_rejects_elicitation_create() {
    // Before #875 the unsupported gap list rejected `elicitation/create`
    // with a -32601 + an mcp.unsupportedFeature `data` payload. Now we
    // implement bidirectional elicitation, so it's removed from that
    // list. The server itself doesn't expect inbound elicitation
    // requests (clients send the *response* not a fresh request), so
    // the dispatcher's plain "Method not found" still fires — but the
    // unsupported-feature data payload should be gone.
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
            crate::jsonrpc::request(7, "elicitation/create", serde_json::json!({})),
            &mut vm,
        )
        .await
        .expect("response");
    assert!(response.get("result").is_none());
    assert_eq!(response["error"]["code"], serde_json::json!(-32601));
    assert!(
        response["error"].get("data").is_none(),
        "expected no `data` payload now that elicitation is removed from the gap list, got {response:?}"
    );
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
