use super::{
    apply_tool_search_native_injection_typed, defer_loading_registry, extract_deferred_tool_names,
    json, sample_tool_registry, vm_bool, vm_dict, vm_list, vm_str, vm_tools_to_native,
};
use crate::llm::provider::NativeToolSearchShape;
use std::collections::BTreeMap;
use std::rc::Rc;

#[test]
fn vm_tools_to_native_emits_defer_loading_for_anthropic() {
    let registry = defer_loading_registry();
    let tools = vm_tools_to_native(&registry, "anthropic", "claude-opus-4-7")
        .expect("anthropic native tools");
    // Eager `look` tool has no defer_loading key.
    let look = tools
        .iter()
        .find(|tool| tool.get("name").and_then(|value| value.as_str()) == Some("look"))
        .expect("look tool present");
    assert!(look.get("defer_loading").is_none());
    // Deferred `deploy` tool carries `defer_loading: true`.
    let deploy = tools
        .iter()
        .find(|tool| tool.get("name").and_then(|value| value.as_str()) == Some("deploy"))
        .expect("deploy tool present");
    assert_eq!(
        deploy
            .get("defer_loading")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}

#[test]
fn vm_tools_to_native_uses_model_capability_shape_for_bedrock_claude() {
    let registry = sample_tool_registry();
    let tools = vm_tools_to_native(
        &registry,
        "bedrock",
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
    )
    .expect("bedrock claude native tools");

    assert_eq!(tools[0]["name"].as_str(), Some("edit"));
    assert!(tools[0].get("input_schema").is_some());
    assert!(
        tools[0].get("function").is_none(),
        "Bedrock-hosted Claude should use Anthropic native tool shape"
    );
}

#[test]
fn vm_tools_to_native_emits_namespace_for_openai_compat() {
    // `namespace` on a tool entry (harn#71) flows through to the
    // OpenAI-shape wrapper alongside `defer_loading`. Provider request
    // builders may strip it from outbound HTTP bodies, but keeping it
    // in the native tool value preserves transcript and tool-search
    // grouping fidelity.
    let mut deferred_params = BTreeMap::new();
    deferred_params.insert("env".to_string(), vm_str("string"));
    let deferred = vm_dict(&[
        ("name", vm_str("deploy")),
        ("description", vm_str("Deploy the app")),
        ("parameters", super::VmValue::Dict(Rc::new(deferred_params))),
        ("defer_loading", vm_bool(true)),
        ("namespace", vm_str("ops")),
    ]);
    let registry = vm_list(vec![deferred]);

    let openai = vm_tools_to_native(&registry, "openai", "gpt-5.4").expect("openai native tools");
    assert_eq!(openai[0]["namespace"].as_str(), Some("ops"));
    assert_eq!(openai[0]["defer_loading"].as_bool(), Some(true));

    let anthropic = vm_tools_to_native(&registry, "anthropic", "claude-opus-4-7")
        .expect("anthropic native tools");
    assert_eq!(
        anthropic[0]["namespace"].as_str(),
        Some("ops"),
        "namespace survives Anthropic passthrough (harmlessly ignored by API)"
    );
}

#[test]
fn vm_tools_to_native_emits_defer_loading_for_openai_compat() {
    // OpenAI-shape tools place the flag at the wrapper level (not inside
    // `function`) so harn#71's Responses-API path can read it uniformly
    // without re-walking. The OpenAI-compatible request builder strips
    // it from providers that do not expose native tool-search extensions.
    let registry = defer_loading_registry();
    let tools = vm_tools_to_native(&registry, "openai", "gpt-5.4").expect("openai native tools");
    let deploy = tools
        .iter()
        .find(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                == Some("deploy")
        })
        .expect("deploy tool present");
    assert_eq!(
        deploy
            .get("defer_loading")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}

#[test]
fn vm_tools_to_native_preserves_rich_parameter_schema_for_openai_compat() {
    let registry = sample_tool_registry();
    let tools = vm_tools_to_native(&registry, "openai", "gpt-5.4").expect("openai native tools");
    let edit = tools
        .iter()
        .find(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                == Some("edit")
        })
        .expect("edit tool present");

    let parameters = &edit["function"]["parameters"];
    let properties = parameters["properties"]
        .as_object()
        .expect("properties object");

    assert_eq!(properties["action"]["type"].as_str(), Some("string"));
    assert_eq!(
        properties["action"]["enum"],
        json!(["create", "patch", "replace_body"])
    );
    assert_eq!(
        properties["path"]["description"].as_str(),
        Some("Repo-relative path.")
    );
    assert_eq!(properties["ops"]["type"].as_str(), Some("array"));
    assert_eq!(properties["content"]["type"].as_str(), Some("string"));
    assert!(
        properties["content"].get("required").is_none(),
        "per-property Harn `required` marker must not leak into JSON Schema"
    );
    assert_eq!(parameters["required"], json!(["action", "path"]));
}

#[test]
fn vm_tools_to_native_normalizes_nested_harn_type_aliases() {
    let mut params = BTreeMap::new();
    params.insert(
        "filters".to_string(),
        vm_dict(&[
            ("type", vm_str("dict")),
            (
                "properties",
                vm_dict(&[
                    ("enabled", vm_dict(&[("type", vm_str("bool"))])),
                    (
                        "scores",
                        vm_dict(&[
                            ("type", vm_str("list")),
                            ("items", vm_dict(&[("type", vm_str("int"))])),
                        ]),
                    ),
                ]),
            ),
        ]),
    );
    let tool = vm_dict(&[
        ("name", vm_str("rank")),
        ("description", vm_str("Rank results")),
        ("parameters", super::VmValue::Dict(Rc::new(params))),
    ]);
    let registry = vm_list(vec![tool]);

    let tools = vm_tools_to_native(&registry, "openai", "gpt-5.4").expect("openai native tools");
    let filters = &tools[0]["function"]["parameters"]["properties"]["filters"];
    assert_eq!(filters["type"].as_str(), Some("object"));
    assert_eq!(
        filters["properties"]["enabled"]["type"].as_str(),
        Some("boolean")
    );
    assert_eq!(
        filters["properties"]["scores"]["type"].as_str(),
        Some("array")
    );
    assert_eq!(
        filters["properties"]["scores"]["items"]["type"].as_str(),
        Some("integer")
    );
}

#[test]
fn vm_tools_to_native_preserves_json_schema_required_arrays() {
    let mut params = BTreeMap::new();
    params.insert(
        "payload".to_string(),
        vm_dict(&[
            ("type", vm_str("dict")),
            (
                "properties",
                vm_dict(&[
                    ("id", vm_dict(&[("type", vm_str("str"))])),
                    ("count", vm_dict(&[("type", vm_str("int"))])),
                ]),
            ),
            ("required", vm_list(vec![vm_str("id")])),
        ]),
    );
    let tool = vm_dict(&[
        ("name", vm_str("submit")),
        ("description", vm_str("Submit payload")),
        ("parameters", super::VmValue::Dict(Rc::new(params))),
    ]);
    let registry = vm_list(vec![tool]);

    let tools = vm_tools_to_native(&registry, "openai", "gpt-5.4").expect("openai native tools");
    let payload = &tools[0]["function"]["parameters"]["properties"]["payload"];
    assert_eq!(payload["type"].as_str(), Some("object"));
    assert_eq!(payload["required"], json!(["id"]));
    assert_eq!(
        payload["properties"]["count"]["type"].as_str(),
        Some("integer")
    );
}

#[test]
fn extract_deferred_tool_names_walks_both_wire_shapes() {
    let anthropic = vec![
        json!({"name": "look"}),
        json!({"name": "deploy", "defer_loading": true}),
    ];
    assert_eq!(
        extract_deferred_tool_names(&anthropic),
        vec!["deploy".to_string()]
    );

    let openai = vec![
        json!({"type": "function", "function": {"name": "look"}}),
        json!({
            "type": "function",
            "function": {"name": "deploy"},
            "defer_loading": true,
        }),
    ];
    assert_eq!(
        extract_deferred_tool_names(&openai),
        vec!["deploy".to_string()]
    );
}

#[test]
fn apply_tool_search_native_injection_prepends_meta_tool() {
    let mut tools: Option<Vec<serde_json::Value>> =
        Some(vec![json!({"name": "look"}), json!({"name": "deploy"})]);
    apply_tool_search_native_injection_typed(
        &mut tools,
        NativeToolSearchShape::Anthropic,
        "bm25",
        "hosted",
    );
    let tools = tools.expect("tools still set");
    assert_eq!(tools.len(), 3, "search tool prepended");
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_bm25_20251119"),
        "bm25 variant uses the documented type string"
    );
    assert_eq!(tools[0]["name"].as_str(), Some("tool_search_tool_bm25"));
}

#[test]
fn apply_tool_search_native_injection_regex_variant() {
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![json!({"name": "look"})]);
    apply_tool_search_native_injection_typed(
        &mut tools,
        NativeToolSearchShape::Anthropic,
        "regex",
        "hosted",
    );
    let tools = tools.unwrap();
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_regex_20251119")
    );
    assert_eq!(tools[0]["name"].as_str(), Some("tool_search_tool_regex"));
}

#[test]
fn apply_tool_search_native_injection_emits_openai_shape_for_non_anthropic() {
    // OpenAI's native `tool_search` meta-tool (harn#71) uses a flat
    // `{"type": "tool_search", "mode": "hosted"}` shape, distinct from
    // Anthropic's versioned `tool_search_tool_*_20251119` block.
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![json!({"name": "look"})]);
    apply_tool_search_native_injection_typed(
        &mut tools,
        NativeToolSearchShape::OpenAi,
        "bm25",
        "hosted",
    );
    let tools = tools.unwrap();
    assert_eq!(tools.len(), 2, "OpenAI meta-tool prepended");
    assert_eq!(tools[0]["type"].as_str(), Some("tool_search"));
    assert_eq!(tools[0]["mode"].as_str(), Some("hosted"));
    assert!(
        tools[0].get("name").is_none(),
        "OpenAI meta-tool has no `name` field (that's an Anthropic detail)"
    );
    assert_eq!(tools[1]["name"].as_str(), Some("look"));
}

#[test]
fn apply_tool_search_native_injection_openai_collects_namespaces() {
    // When deferred tools declare a `namespace`, OpenAI's meta-tool
    // carries the distinct set so the server can group them.
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![
        json!({
            "type": "function",
            "function": {"name": "deploy_api"},
            "namespace": "ops",
        }),
        json!({
            "type": "function",
            "function": {"name": "deploy_web"},
            "namespace": "ops",
        }),
        json!({
            "type": "function",
            "function": {"name": "lookup_account"},
            "namespace": "crm",
        }),
    ]);
    apply_tool_search_native_injection_typed(
        &mut tools,
        NativeToolSearchShape::OpenAi,
        "bm25",
        "hosted",
    );
    let tools = tools.unwrap();
    let namespaces = tools[0]["namespaces"]
        .as_array()
        .expect("namespaces present");
    let names: Vec<&str> = namespaces
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert_eq!(names, vec!["crm", "ops"], "sorted + deduped");
}

#[test]
fn apply_tool_search_native_injection_creates_list_when_empty() {
    let mut tools: Option<Vec<serde_json::Value>> = None;
    apply_tool_search_native_injection_typed(
        &mut tools,
        NativeToolSearchShape::Anthropic,
        "bm25",
        "hosted",
    );
    let tools = tools.expect("tools populated");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_bm25_20251119")
    );
}
