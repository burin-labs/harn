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
use super::{McpScriptRegistry, McpServerMetadata, McpToolSet};

#[path = "tests/application_errors.rs"]
mod application_errors;

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
        invocation_requirement: crate::tool_registry::ToolInvocationRequirement::Direct,
        catalog: crate::tool_registry::ToolCatalogEntry {
            name: name.to_string(),
            title: None,
            description: Some(description.to_string()),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            error_schema: None,
            annotations: None,
            icons: None,
            execution: (task_support != crate::mcp_tasks::McpTaskSupport::Forbidden)
                .then_some(crate::tool_registry::ToolExecution { task_support }),
            governance: crate::tool_registry::ToolGovernance::default(),
            cli: crate::tool_registry::ToolCliSpec {
                command: vec![name.to_string()],
                aliases: Vec::new(),
                hidden: false,
                arguments: std::collections::BTreeMap::new(),
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

fn tool_set(definitions: Vec<McpToolDef>) -> McpToolSet {
    McpToolSet::prepare(definitions).expect("valid test tool catalog")
}

#[test]
fn mcp_tool_set_preparation_reports_invalid_contracts() {
    let mut invalid = tool_def(
        "invalid",
        "Invalid schema",
        None,
        crate::mcp_tasks::McpTaskSupport::Forbidden,
    );
    invalid.catalog.input_schema = serde_json::json!({"type": "not-a-json-schema-type"});

    let error = match McpToolSet::prepare(vec![invalid]) {
        Ok(_) => panic!("invalid schema must fail"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("not-a-json-schema-type"), "{message}");
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
    param_def.put("required", VmValue::Bool(true));
    params.put("path", VmValue::dict(param_def));

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

fn script_registry(tools: McpToolSet) -> McpScriptRegistry {
    McpScriptRegistry::new(
        "test".to_string(),
        tools,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

async fn call(
    registry: &McpScriptRegistry,
    params: serde_json::Value,
    as_task: bool,
) -> serde_json::Value {
    let mut vm = crate::new_vm();
    registry
        .call_tool(&serde_json::json!(1), &params, &mut vm, None, as_task)
        .await
}

#[tokio::test]
async fn registry_projects_extension_metadata_on_tools_and_resource_content() {
    let extension_meta = serde_json::json!({
        "ui": {
            "resourceUri": "ui://example/editor",
            "visibility": ["model", "app"]
        }
    });
    let registry = McpScriptRegistry::new(
        "test".to_string(),
        tool_set(vec![tool_def(
            "open_editor",
            "Open the editor",
            extension_meta.as_object().cloned(),
            crate::mcp_tasks::McpTaskSupport::Forbidden,
        )]),
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
    let surface = registry.surface();
    assert!(surface.serves_mcp_apps);
    assert_eq!(
        surface.tools[0]["_meta"]["ui"]["resourceUri"],
        "ui://example/editor"
    );

    let mut vm = crate::new_vm();
    let content = registry
        .read_resource(
            &serde_json::json!(2),
            &serde_json::json!({"uri": "ui://example/editor"}),
            &mut vm,
        )
        .await;
    assert_eq!(
        content["result"]["contents"][0]["_meta"]["ui"]["prefersBorder"],
        false
    );
}

#[tokio::test]
async fn mcp_audience_blocks_discovery_and_forged_calls() {
    let mut excluded = tool_def(
        "cli_only",
        "CLI-only tool",
        None,
        crate::mcp_tasks::McpTaskSupport::Forbidden,
    );
    excluded.catalog.governance.audiences = vec![crate::tool_registry::ToolAudience::Cli];
    let registry = script_registry(tool_set(vec![excluded]));

    let surface = registry.surface();
    assert!(surface.tools.is_empty());
    assert!(surface.task_support.is_empty());

    let forged = call(
        &registry,
        serde_json::json!({"name": "cli_only", "arguments": {}}),
        false,
    )
    .await;
    assert_eq!(forged["error"]["code"], -32602);
    assert!(forged["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unknown tool"));
    assert!(registry
        .admit_tool(
            &serde_json::json!(2),
            &serde_json::json!({"name": "cli_only", "arguments": {}})
        )
        .is_err());
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

#[test]
fn registry_metadata_overrides_identity() {
    let registry = McpScriptRegistry::new(
        "file-stem".to_string(),
        tool_set(Vec::new()),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_metadata(McpServerMetadata {
        name: Some("test-echo-server".to_string()),
        version: Some("1.0.0".to_string()),
        instructions: Some("Use echo server tools only for conformance.".to_string()),
    });
    let surface = registry.surface();
    assert_eq!(surface.server_name, "test-echo-server");
    assert_eq!(surface.server_version, "1.0.0");
    assert_eq!(
        surface.instructions.as_deref(),
        Some("Use echo server tools only for conformance.")
    );
}

#[tokio::test]
async fn registry_completes_prompt_and_resource_arguments() {
    let mut resource_completions = std::collections::BTreeMap::new();
    resource_completions.insert(
        "key".to_string(),
        McpCompletionSource {
            values: vec!["name".to_string(), "version".to_string()],
            handler: None,
        },
    );
    let registry = McpScriptRegistry::new(
        "test".to_string(),
        tool_set(Vec::new()),
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
                title: Some("Language".to_string()),
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
    let surface = registry.surface();
    assert_eq!(surface.prompts[0]["arguments"][0]["title"], "Language");
    assert_eq!(
        surface.resource_templates[0]["uriTemplate"],
        "config://{key}"
    );

    let mut vm = crate::new_vm();
    let prompt = registry
        .complete(
            &serde_json::json!(2),
            &serde_json::json!({
                "ref": {"type": "ref/prompt", "name": "review"},
                "argument": {"name": "language", "value": "ru"},
            }),
            &mut vm,
        )
        .await;
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

    let resource = registry
        .complete(
            &serde_json::json!(3),
            &serde_json::json!({
                "ref": {"type": "ref/resource", "uri": "config://{key}"},
                "argument": {"name": "key", "value": "ver"},
            }),
            &mut vm,
        )
        .await;
    assert_eq!(
        resource["result"]["completion"]["values"],
        serde_json::json!(["version"])
    );
}

#[tokio::test]
async fn registry_reads_static_resources_and_reports_unknown_uris() {
    let registry = McpScriptRegistry::new(
        "test".to_string(),
        tool_set(Vec::new()),
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
    assert_eq!(registry.surface().resources[0]["uri"], "docs://readme");
    let mut vm = crate::new_vm();
    let read = registry
        .read_resource(
            &serde_json::json!(1),
            &serde_json::json!({"uri": "docs://readme"}),
            &mut vm,
        )
        .await;
    assert_eq!(read["result"]["contents"][0]["text"], "hello");
    let missing = registry
        .read_resource(
            &serde_json::json!(2),
            &serde_json::json!({"uri": "docs://missing"}),
            &mut vm,
        )
        .await;
    assert_eq!(missing["error"]["code"], -32002);
}

fn task_capable_registry(support: crate::mcp_tasks::McpTaskSupport) -> McpScriptRegistry {
    let mut tool = tool_def("slow_report", "Build a report", None, support);
    tool.catalog.output_schema = Some(serde_json::json!({"type": "null"}));
    script_registry(tool_set(vec![tool]))
}

#[tokio::test]
async fn tool_contract_validation_precedes_execution() {
    let mut tool = tool_def(
        "bounded_report",
        "Build a bounded report",
        None,
        crate::mcp_tasks::McpTaskSupport::Optional,
    );
    tool.catalog.input_schema = serde_json::json!({
        "type": "object",
        "properties": {"count": {"type": "integer", "minimum": 1}},
        "required": ["count"],
        "additionalProperties": false,
    });
    tool.catalog.output_schema = Some(serde_json::json!({"type": "integer"}));
    // empty_closure returns nil, intentionally violating the declared output.
    let registry = script_registry(tool_set(vec![tool]));

    let invalid =
        serde_json::json!({"name": "bounded_report", "arguments": {"count": 0, "ignored": true}});
    let refused = registry
        .admit_tool(&serde_json::json!(1), &invalid)
        .expect_err("invalid input is refused before any task exists");
    assert_eq!(refused["error"]["code"], serde_json::json!(-32602));
    assert_eq!(
        call(&registry, invalid, false).await["error"]["code"],
        serde_json::json!(-32602)
    );

    let valid = serde_json::json!({"name": "bounded_report", "arguments": {"count": 1}});
    assert!(registry.admit_tool(&serde_json::json!(2), &valid).is_ok());
    for as_task in [false, true] {
        let invalid_output = call(&registry, valid.clone(), as_task).await;
        assert_eq!(invalid_output["result"]["isError"], serde_json::json!(true));
        assert!(invalid_output["result"]["structuredContent"].is_null());
    }
}

/// Inline and task completion share one `CallToolResult`; the server files
/// the task's copy unchanged.
#[tokio::test]
async fn a_declared_tool_produces_the_same_result_inline_and_as_a_task() {
    let registry = task_capable_registry(crate::mcp_tasks::McpTaskSupport::Optional);
    let surface = registry.surface();
    assert_eq!(
        surface.task_support.get("slow_report"),
        Some(&crate::mcp_tasks::McpTaskSupport::Optional)
    );
    assert_eq!(
        surface.tools[0]["outputSchema"],
        serde_json::json!({
            "type": "object",
            "properties": {"result": {"type": "null"}},
            "required": ["result"],
            "additionalProperties": false,
        })
    );

    let params = serde_json::json!({"name": "slow_report", "arguments": {}});
    let inline = call(&registry, params.clone(), false).await;
    let task = call(&registry, params, true).await;
    assert_eq!(inline["result"], task["result"]);
    assert_eq!(
        task["result"]["structuredContent"],
        serde_json::json!({"result": null})
    );
    let output_validator = jsonschema::draft202012::new(&surface.tools[0]["outputSchema"])
        .expect("advertised output schema");
    assert!(output_validator.is_valid(&task["result"]["structuredContent"]));
}

/// The stable tasks extension makes the server the sole per-request decider;
/// retired tool-level task metadata must not leak back into discovery.
#[test]
fn tools_list_omits_retired_tool_level_task_support() {
    for support in [
        crate::mcp_tasks::McpTaskSupport::Optional,
        crate::mcp_tasks::McpTaskSupport::Required,
        crate::mcp_tasks::McpTaskSupport::Forbidden,
    ] {
        let surface = task_capable_registry(support).surface();
        assert!(surface.tools[0].get("execution").is_none());
        assert_eq!(surface.task_support.get("slow_report"), Some(&support));
    }
}
