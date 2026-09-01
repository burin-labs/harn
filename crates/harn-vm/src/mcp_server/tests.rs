use crate::value::VmDictExt;
use std::sync::Arc;

use crate::chunk::{Chunk, CompiledFunction};
use crate::value::{VmClosure, VmEnv, VmValue};

use super::convert::{annotations_to_json, prompt_value_to_messages, vm_value_to_json};
use super::defs::{
    McpCompletionSource, McpPromptArgDef, McpPromptDef, McpResourceDef, McpResourceTemplateDef,
    McpToolDef,
};
use super::tool_registry_to_mcp_tools;
use super::tools_schema::params_to_json_schema;
use super::uri::match_uri_template;
use super::{McpServer, McpServerMetadata};

fn empty_closure(name: &str) -> VmClosure {
    VmClosure {
        func: Arc::new(CompiledFunction {
            name: crate::value::HarnStr::from(name),
            type_params: Vec::new(),
            nominal_type_names: Vec::new(),
            params: Vec::new(),
            default_start: None,
            chunk: Arc::new(Chunk::new()),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks: false,
        }),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    }
}

fn tool_def(
    name: &str,
    description: &str,
    meta: Option<serde_json::Map<String, serde_json::Value>>,
    task_support: crate::mcp_tasks::McpTaskSupport,
) -> McpToolDef {
    McpToolDef {
        catalog: crate::tool_registry::ToolCatalogEntry {
            name: name.to_string(),
            title: None,
            description: Some(description.to_string()),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            icons: None,
            execution: (task_support != crate::mcp_tasks::McpTaskSupport::Forbidden)
                .then_some(crate::tool_registry::ToolExecution { task_support }),
            governance: crate::tool_registry::ToolGovernance::default(),
            cli: crate::tool_registry::ToolCliSpec {
                command: vec![name.to_string()],
                hidden: false,
            },
            namespace: None,
            defer_loading: false,
            source: None,
            policy: None,
            meta: meta.map(|meta| meta.into_iter().collect()),
        },
        handler: empty_closure(name),
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
    let mut params = crate::value::DictMap::new();
    let mut param_def = crate::value::DictMap::new();
    param_def.put_str("type", "string");
    param_def.put_str("description", "A file path");
    param_def.insert(crate::value::intern_key("required"), VmValue::Bool(true));
    params.insert(crate::value::intern_key("path"), VmValue::dict(param_def));

    let schema = params_to_json_schema(Some(&VmValue::dict(params)));
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
    let mut params = crate::value::DictMap::new();
    params.put_str("query", "string");
    let schema = params_to_json_schema(Some(&VmValue::dict(params)));
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
    let mut registry = crate::value::DictMap::new();
    registry.insert(
        "_type".into(),
        VmValue::String(arcstr::ArcStr::from("tool_registry")),
    );
    registry.insert(
        "tools".into(),
        VmValue::List(std::sync::Arc::new(Vec::new())),
    );
    let result = tool_registry_to_mcp_tools(&VmValue::dict(registry));
    assert!(result.unwrap().is_empty());
}

#[test]
fn structured_tool_values_project_as_json_objects() {
    let value = VmValue::dict({
        let mut update = crate::value::DictMap::new();
        update.put_str("schema", "harn.ui_update.v1");
        update.insert(
            "effects".into(),
            VmValue::List(Arc::new(vec![VmValue::dict({
                let mut effect = crate::value::DictMap::new();
                effect.put_str("kind", "capture_canvas");
                effect
            })])),
        );
        update
    });

    assert_eq!(
        vm_value_to_json(&value),
        serde_json::json!({
            "schema": "harn.ui_update.v1",
            "effects": [{"kind": "capture_canvas"}]
        })
    );
}

#[test]
fn test_tool_registry_to_mcp_tools_preserves_metadata() {
    let handler = VmValue::Closure(Arc::new(empty_closure("echo")));

    let mut annotations = crate::value::DictMap::new();
    annotations.insert("readOnlyHint".into(), VmValue::Bool(true));
    annotations.insert("idempotentHint".into(), VmValue::Bool(true));

    let icon = VmValue::dict({
        let mut icon = crate::value::DictMap::new();
        icon.insert(
            "src".into(),
            VmValue::String(arcstr::ArcStr::from("https://example.com/tool.png")),
        );
        icon.insert(
            "mimeType".into(),
            VmValue::String(arcstr::ArcStr::from("image/png")),
        );
        icon
    });

    let mut tool = crate::value::DictMap::new();
    tool.insert("name".into(), VmValue::String(arcstr::ArcStr::from("echo")));
    tool.insert(
        "title".into(),
        VmValue::String(arcstr::ArcStr::from("Echo")),
    );
    tool.insert(
        "description".into(),
        VmValue::String(arcstr::ArcStr::from("Echo input")),
    );
    tool.insert("handler".into(), handler);
    tool.insert(
        "parameters".into(),
        VmValue::dict(crate::value::DictMap::new()),
    );
    tool.insert("annotations".into(), VmValue::dict(annotations));
    tool.insert(
        "meta".into(),
        VmValue::dict({
            let mut ui = crate::value::DictMap::new();
            ui.put_str("resourceUri", "ui://example/editor");
            let mut meta = crate::value::DictMap::new();
            meta.insert("ui".into(), VmValue::dict(ui));
            meta
        }),
    );
    tool.insert(
        "icons".into(),
        VmValue::List(std::sync::Arc::new(vec![icon])),
    );
    tool.insert(
        "outputSchema".into(),
        VmValue::dict({
            let mut schema = crate::value::DictMap::new();
            schema.insert(
                "type".into(),
                VmValue::String(arcstr::ArcStr::from("string")),
            );
            schema
        }),
    );

    let mut registry = crate::value::DictMap::new();
    registry.insert(
        "_type".into(),
        VmValue::String(arcstr::ArcStr::from("tool_registry")),
    );
    registry.insert(
        "tools".into(),
        VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(tool),
        )])),
    );

    let tools = tool_registry_to_mcp_tools(&VmValue::dict(registry)).unwrap();
    assert_eq!(tools[0].catalog.title.as_deref(), Some("Echo"));
    assert_eq!(
        tools[0]
            .catalog
            .annotations
            .as_ref()
            .unwrap()
            .read_only_hint,
        Some(true)
    );
    assert_eq!(
        tools[0].catalog.icons.as_ref().unwrap()[0].src,
        "https://example.com/tool.png"
    );
    assert_eq!(
        tools[0].catalog.output_schema.as_ref().unwrap()["type"],
        "string"
    );
    assert_eq!(
        tools[0].catalog.meta.as_ref().unwrap()["ui"]["resourceUri"],
        "ui://example/editor"
    );
}

#[tokio::test]
async fn server_projects_extension_metadata_on_tools_and_resource_content() {
    let extension_meta = serde_json::json!({
        "ui": {
            "resourceUri": "ui://example/editor",
            "visibility": ["model", "app"]
        }
    });
    let server = McpServer::new(
        "test".to_string(),
        vec![tool_def(
            "open_editor",
            "Open the editor",
            extension_meta.as_object().cloned(),
            crate::mcp_tasks::McpTaskSupport::Forbidden,
        )],
        vec![McpResourceDef {
            uri: "ui://example/editor".to_string(),
            name: "Editor".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/html;profile=mcp-app".to_string()),
            meta: Some(serde_json::json!({
                "ui": {"csp": {"connectDomains": []}, "prefersBorder": false}
            })),
            text: "<!doctype html><html></html>".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let discovered = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                0,
                crate::mcp_protocol::METHOD_SERVER_DISCOVER,
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .unwrap();
    assert_eq!(
        discovered["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/ui"]
            ["mimeTypes"][0],
        "text/html;profile=mcp-app"
    );

    let tools = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/list",
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .unwrap();
    assert_eq!(
        tools["result"]["tools"][0]["_meta"]["ui"]["resourceUri"],
        "ui://example/editor"
    );

    let content = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                "resources/read",
                stable_metadata_params(serde_json::json!({"uri": "ui://example/editor"})),
            ),
            &mut vm,
        )
        .await
        .unwrap();
    assert_eq!(
        content["result"]["contents"][0]["_meta"]["ui"]["prefersBorder"],
        false
    );
}

#[test]
fn test_prompt_value_to_messages_string() {
    let msgs = prompt_value_to_messages(&VmValue::String(arcstr::ArcStr::from("hello")));
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"]["text"], "hello");
}

#[test]
fn test_prompt_value_to_messages_list() {
    let items = vec![
        VmValue::dict({
            let mut d = crate::value::DictMap::new();
            d.insert("role".into(), VmValue::String(arcstr::ArcStr::from("user")));
            d.insert(
                "content".into(),
                VmValue::String(arcstr::ArcStr::from("hi")),
            );
            d
        }),
        VmValue::dict({
            let mut d = crate::value::DictMap::new();
            d.insert(
                "role".into(),
                VmValue::String(arcstr::ArcStr::from("assistant")),
            );
            d.insert(
                "content".into(),
                VmValue::String(arcstr::ArcStr::from("hello")),
            );
            d
        }),
    ];
    let msgs = prompt_value_to_messages(&VmValue::List(std::sync::Arc::new(items)));
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1]["role"], "assistant");
}

#[test]
fn test_prompt_value_to_messages_preserves_image_content() {
    let items = vec![VmValue::dict({
        let mut image = crate::value::DictMap::new();
        image.insert(
            "type".into(),
            VmValue::String(arcstr::ArcStr::from("image")),
        );
        image.insert(
            "data".into(),
            VmValue::String(arcstr::ArcStr::from("ZmFrZQ==")),
        );
        image.insert(
            "mimeType".into(),
            VmValue::String(arcstr::ArcStr::from("image/png")),
        );

        let mut message = crate::value::DictMap::new();
        message.insert("role".into(), VmValue::String(arcstr::ArcStr::from("user")));
        message.insert("content".into(), VmValue::dict(image));
        message
    })];
    let msgs = prompt_value_to_messages(&VmValue::List(std::sync::Arc::new(items)));
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
    let mut d = crate::value::DictMap::new();
    d.insert(
        "title".into(),
        VmValue::String(arcstr::ArcStr::from("My Tool")),
    );
    d.insert("readOnlyHint".into(), VmValue::Bool(true));
    d.insert("destructiveHint".into(), VmValue::Bool(false));
    let json = annotations_to_json(&VmValue::dict(d)).unwrap();
    assert_eq!(json["title"], "My Tool");
    assert_eq!(json["readOnlyHint"], true);
    assert_eq!(json["destructiveHint"], false);
}

#[test]
fn test_annotations_empty_returns_none() {
    let d = crate::value::DictMap::new();
    assert!(annotations_to_json(&VmValue::dict(d)).is_none());
}

#[tokio::test]
async fn server_advertises_stable_resource_and_task_capabilities() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            meta: None,
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
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("response");

    assert_eq!(
        response["result"]["capabilities"]["resources"],
        serde_json::json!({})
    );
    assert_eq!(
        response["result"]["capabilities"]["extensions"][crate::mcp_protocol::TASKS_EXTENSION_ID],
        serde_json::json!({})
    );
}

#[tokio::test]
async fn server_metadata_overrides_discover() {
    let server = McpServer::new(
        "file-stem".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_metadata(McpServerMetadata {
        name: Some("test-echo-server".to_string()),
        version: Some("1.0.0".to_string()),
        instructions: Some("Use echo server tools only for conformance.".to_string()),
    });
    let mut vm = crate::Vm::new();

    let discover = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                crate::mcp_protocol::METHOD_SERVER_DISCOVER,
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("discover response");
    assert_eq!(
        discover["result"]["instructions"],
        serde_json::json!("Use echo server tools only for conformance.")
    );
    assert_eq!(
        discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"],
        serde_json::json!({"name": "test-echo-server", "version": "1.0.0"})
    );
}

#[tokio::test]
async fn server_completion_complete_returns_prompt_and_resource_suggestions() {
    let mut resource_completions = std::collections::BTreeMap::new();
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

    let discover = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                crate::mcp_protocol::METHOD_SERVER_DISCOVER,
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert!(discover["result"]["capabilities"]["completions"].is_object());

    let prompt = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                crate::mcp_protocol::METHOD_COMPLETION_COMPLETE,
                stable_metadata_params(serde_json::json!({
                    "ref": {"type": "ref/prompt", "name": "review"},
                    "argument": {"name": "language", "value": "ru"},
                })),
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
                stable_metadata_params(serde_json::json!({
                    "ref": {"type": "ref/resource", "uri": "config://{key}"},
                    "argument": {"name": "key", "value": "ver"},
                })),
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
                crate::jsonrpc::request(7, method, stable_metadata_params(serde_json::json!({}))),
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
async fn server_tool_call_ignores_client_task_support_and_executes_inline() {
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
                stable_metadata_params(serde_json::json!({
                    "name": "missing",
                    "arguments": {},
                    "_meta": {
                        crate::mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION:
                            crate::mcp_protocol::PROTOCOL_VERSION,
                        crate::mcp_protocol::MCP_META_KEY_CLIENT_INFO:
                            {"name": "stable-client", "version": "1.0"},
                        crate::mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES: {
                            "extensions": {crate::mcp_protocol::TASKS_EXTENSION_ID: {}}
                        }
                    }
                })),
            ),
            &mut vm,
        )
        .await
        .expect("response");

    assert_eq!(response["error"]["code"], serde_json::json!(-32602));
    assert_eq!(response["error"]["message"], "Unknown tool: missing");
}

fn stable_metadata_params(rest: serde_json::Value) -> serde_json::Value {
    let mut object = match rest {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        other => serde_json::Map::from_iter([("value".to_string(), other)]),
    };
    let meta = object
        .entry("_meta".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let meta = meta.as_object_mut().expect("_meta must be an object");
    meta.entry(crate::mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION)
        .or_insert_with(|| serde_json::json!(crate::mcp_protocol::PROTOCOL_VERSION));
    meta.entry(crate::mcp_protocol::MCP_META_KEY_CLIENT_INFO)
        .or_insert_with(|| serde_json::json!({"name": "stable-client", "version": "1.0"}));
    meta.entry(crate::mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES)
        .or_insert_with(|| serde_json::json!({}));
    serde_json::Value::Object(object)
}

#[tokio::test]
async fn server_discover_returns_capabilities_and_supported_versions() {
    let server = McpServer::new(
        "stable-test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            meta: None,
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
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        response["result"]["resultType"],
        serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(response["result"]["ttlMs"], serde_json::json!(0));
    assert_eq!(
        response["result"]["cacheScope"],
        serde_json::json!("private")
    );
    let supported = response["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions");
    assert_eq!(
        supported.as_slice(),
        [serde_json::json!(crate::mcp_protocol::PROTOCOL_VERSION)]
    );
    assert_eq!(
        response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "stable-test"
    );
}

#[tokio::test]
async fn server_rejects_request_with_unsupported_protocol_version_metadata() {
    let server = McpServer::new(
        "stable-test".to_string(),
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
                        crate::mcp_protocol::MCP_META_KEY_PROTOCOL_VERSION: "2099-01-01"
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
        .any(|v| v == &serde_json::json!(crate::mcp_protocol::PROTOCOL_VERSION)));
}

#[tokio::test]
async fn server_stable_tools_list_emits_result_type_and_cache_hint() {
    let server = McpServer::new(
        "stable-test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            meta: None,
            text: "hello".to_string(),
        }],
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();

    let stable = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "resources/list",
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("stable response");
    assert_eq!(
        stable["result"]["resultType"],
        serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE)
    );
    assert_eq!(
        stable["result"]["cacheScope"],
        serde_json::json!(crate::mcp_protocol::DEFAULT_LIST_CACHE_SCOPE)
    );
    assert_eq!(
        stable["result"]["ttlMs"],
        serde_json::json!(crate::mcp_protocol::DEFAULT_LIST_CACHE_TTL_MS)
    );
}

#[tokio::test]
async fn server_stable_non_list_methods_carry_result_type_envelope() {
    let server = McpServer::new(
        "stable-test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let params = stable_metadata_params(serde_json::json!({}));
    let response = server
        .handle_json_rpc(crate::jsonrpc::request(1, "ping", params), &mut vm)
        .await
        .expect("response");
    assert_eq!(
        response["result"]["resultType"],
        serde_json::json!(crate::mcp_protocol::RESULT_TYPE_COMPLETE)
    );
}

#[tokio::test]
async fn script_server_initializes_released_clients_without_stable_request_metadata() {
    let server = McpServer::new(
        "released-client-test".to_string(),
        vec![tool_def(
            "render_fixture",
            "Render a fixture",
            None,
            crate::mcp_tasks::McpTaskSupport::Forbidden,
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let initialized = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "released-client", "version": "1"},
                }),
            ),
            &mut vm,
        )
        .await
        .expect("initialize response");
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");

    let listed = server
        .handle_json_rpc(
            crate::jsonrpc::request(2, "tools/list", serde_json::json!({})),
            &mut vm,
        )
        .await
        .expect("tools/list response");
    assert_eq!(listed["result"]["tools"][0]["name"], "render_fixture");
    assert!(listed["result"].get("resultType").is_none());
}

#[tokio::test]
async fn reloadable_script_server_advertises_tool_list_changes() {
    let server = McpServer::new(
        "reloadable-test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_list_changes(true);
    let mut vm = crate::Vm::new();
    let initialized = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "reload-client", "version": "1"},
                }),
            ),
            &mut vm,
        )
        .await
        .expect("initialize response");

    assert_eq!(
        initialized["result"]["capabilities"]["tools"]["listChanged"],
        true
    );
    assert_eq!(
        initialized["result"]["capabilities"]["resources"]["listChanged"],
        true
    );
    assert_eq!(
        initialized["result"]["capabilities"]["prompts"]["listChanged"],
        true
    );
}

#[tokio::test]
async fn server_stable_resources_read_emits_cache_hint() {
    let server = McpServer::new(
        "stable-test".to_string(),
        Vec::new(),
        vec![McpResourceDef {
            uri: "docs://readme".to_string(),
            name: "README".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/plain".to_string()),
            meta: None,
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
                stable_metadata_params(serde_json::json!({"uri": "docs://readme"})),
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
async fn server_task_endpoints_report_a_genuinely_missing_task() {
    let server = McpServer::new(
        "test".to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut vm = crate::Vm::new();
    let missing = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                "tasks/get",
                stable_metadata_params(serde_json::json!({"taskId": "missing"})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(missing["error"]["code"], serde_json::json!(-32602));
}

fn task_capable_server(support: crate::mcp_tasks::McpTaskSupport) -> McpServer {
    let mut tool = tool_def("slow_report", "Build a report", None, support);
    tool.catalog.output_schema = Some(serde_json::json!({"type": "null"}));
    McpServer::new(
        "test".to_string(),
        vec![tool],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

/// `_meta` for a client that carries the tasks extension.
fn task_client_params(rest: serde_json::Value) -> serde_json::Value {
    let mut params = stable_metadata_params(rest);
    params["_meta"][crate::mcp_protocol::MCP_META_KEY_CLIENT_CAPABILITIES] = serde_json::json!({
        "extensions": {crate::mcp_protocol::TASKS_EXTENSION_ID: {}}
    });
    params
}

/// The whole point of the extension from a client's side: hand back an id,
/// then answer questions about it. The server used to advertise the capability
/// and answer every one of these with `task not found`, which a polling client
/// cannot distinguish from its task having been dropped.
#[tokio::test]
async fn a_declared_tool_hands_back_a_task_a_client_can_actually_read() {
    let server = task_capable_server(crate::mcp_tasks::McpTaskSupport::Optional);
    let mut vm = crate::Vm::new();

    let listed = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                0,
                "tools/list",
                stable_metadata_params(serde_json::json!({})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(
        listed["result"]["tools"][0]["outputSchema"],
        serde_json::json!({
            "type": "object",
            "properties": {"result": {"type": "null"}},
            "required": ["result"],
            "additionalProperties": false,
        })
    );

    let inline = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                0,
                "tools/call",
                stable_metadata_params(serde_json::json!({"name": "slow_report", "arguments": {}})),
            ),
            &mut vm,
        )
        .await
        .expect("response");

    let created = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/call",
                task_client_params(serde_json::json!({"name": "slow_report", "arguments": {}})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(created["result"]["resultType"], serde_json::json!("task"));
    assert_eq!(created["result"]["status"], serde_json::json!("working"));
    let task_id = created["result"]["taskId"]
        .as_str()
        .expect("a created task carries an id")
        .to_string();

    let read = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                2,
                "tasks/get",
                stable_metadata_params(serde_json::json!({"taskId": task_id})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(read["result"]["taskId"], serde_json::json!(task_id));
    assert_eq!(read["result"]["status"], serde_json::json!("completed"));
    assert!(
        read["result"]["result"].is_object(),
        "a completed task must carry the tool result, got {read}",
    );
    for field in ["content", "isError", "structuredContent"] {
        assert_eq!(
            read["result"]["result"][field], inline["result"][field],
            "inline and task completion must share CallToolResult field {field}"
        );
    }
    assert_eq!(
        read["result"]["result"]["structuredContent"],
        serde_json::json!({"result": null})
    );
    let output_validator =
        jsonschema::draft202012::new(&listed["result"]["tools"][0]["outputSchema"])
            .expect("advertised output schema");
    assert!(output_validator.is_valid(&read["result"]["result"]["structuredContent"]));

    // Cancelling something already terminal has to be refused rather than
    // quietly accepted, or a client cannot tell whether it beat the work.
    let cancel = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                3,
                "tasks/cancel",
                stable_metadata_params(serde_json::json!({"taskId": task_id})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert!(cancel["error"]["message"]
        .as_str()
        .expect("a refused cancel explains itself")
        .contains("already in terminal status 'completed'"));
}

/// Opting in is per tool, so adding the extension cannot change how a tool that
/// never declared it behaves — even for a client that supports tasks.
#[tokio::test]
async fn an_undeclared_tool_still_answers_a_task_capable_client_inline() {
    let server = task_capable_server(crate::mcp_tasks::McpTaskSupport::Forbidden);
    let mut vm = crate::Vm::new();

    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/call",
                task_client_params(serde_json::json!({"name": "slow_report", "arguments": {}})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert!(
        response["result"]["taskId"].is_null(),
        "a forbidden tool must not create a task, got {response}",
    );
    assert_eq!(response["result"]["isError"], serde_json::json!(false));
}

/// `required` is the half that has to be enforced on the call: a client that
/// simply omits the capability must not get the work done synchronously.
#[tokio::test]
async fn a_required_tool_refuses_a_client_that_did_not_ask_for_a_task() {
    let server = task_capable_server(crate::mcp_tasks::McpTaskSupport::Required);
    let mut vm = crate::Vm::new();

    let response = server
        .handle_json_rpc(
            crate::jsonrpc::request(
                1,
                "tools/call",
                stable_metadata_params(serde_json::json!({"name": "slow_report", "arguments": {}})),
            ),
            &mut vm,
        )
        .await
        .expect("response");
    assert_eq!(response["error"]["code"], serde_json::json!(-32602));
    assert!(response["error"]["message"]
        .as_str()
        .expect("the refusal explains itself")
        .contains("must be invoked as a task"));
}

/// A client decides whether to poll from `tools/list`, so the declaration has
/// to reach it there. An undeclared tool stays silent rather than saying
/// `forbidden`, keeping the listing byte-identical for servers that never opt in.
#[tokio::test]
async fn tools_list_reports_which_tools_accept_a_task() {
    let mut vm = crate::Vm::new();
    for (support, expected) in [
        (
            crate::mcp_tasks::McpTaskSupport::Optional,
            serde_json::json!({"taskSupport": "optional"}),
        ),
        (
            crate::mcp_tasks::McpTaskSupport::Required,
            serde_json::json!({"taskSupport": "required"}),
        ),
        (
            crate::mcp_tasks::McpTaskSupport::Forbidden,
            serde_json::Value::Null,
        ),
    ] {
        let listed = task_capable_server(support)
            .handle_json_rpc(
                crate::jsonrpc::request(
                    1,
                    "tools/list",
                    stable_metadata_params(serde_json::json!({})),
                ),
                &mut vm,
            )
            .await
            .expect("response");
        assert_eq!(listed["result"]["tools"][0]["execution"], expected);
    }
}
