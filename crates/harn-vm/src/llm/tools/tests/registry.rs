use super::*;
use std::collections::BTreeMap;
use std::rc::Rc;

#[test]
fn native_schema_ref_resolves_to_component_alias() {
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "touch",
            "description": "Touch a file path.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "$ref": "#/components/schemas/FilePath" }
                },
                "required": ["path"]
            }
        },
        "components": {
            "schemas": {
                "FilePath": {
                    "type": "string",
                    "description": "Repo-relative path"
                }
            }
        }
    })];
    let (schemas, registry) = collect_tool_schemas_with_registry(None, Some(&native_tools));
    assert_eq!(schemas.len(), 1);
    let aliases = registry.render_aliases();
    assert!(
        aliases.contains("type FilePath = string;"),
        "expected type alias for FilePath: {aliases}"
    );
    let prompt = build_tool_calling_contract_prompt(None, Some(&native_tools), "text", false, None);
    assert!(
        prompt.contains("type FilePath = string;"),
        "prompt missing alias: {prompt}"
    );
    assert!(
        prompt.contains("path: FilePath"),
        "signature should reference alias: {prompt}"
    );
}

#[test]
fn component_registry_handles_recursive_refs_without_looping() {
    let mut registry = ComponentRegistry::default();
    let root = json!({
        "components": {
            "schemas": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "children": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Node" }
                        }
                    }
                }
            }
        }
    });
    let node_schema = root["components"]["schemas"]["Node"].clone();
    let _ty = json_schema_to_type_expr(&node_schema, &root, &mut registry);
    let _ = registry.render_aliases();
}

#[test]
fn vm_tools_to_native_emits_defer_loading_for_anthropic() {
    let registry = defer_loading_registry();
    let tools = vm_tools_to_native(&registry, "anthropic").expect("anthropic native tools");
    let look = tools
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("look"))
        .expect("look tool present");
    assert!(look.get("defer_loading").is_none());
    let deploy = tools
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("deploy"))
        .expect("deploy tool present");
    assert_eq!(
        deploy.get("defer_loading").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn vm_tools_to_native_emits_namespace_for_openai_compat() {
    let mut deferred_params = BTreeMap::new();
    deferred_params.insert("env".to_string(), vm_str("string"));
    let deferred = vm_dict(&[
        ("name", vm_str("deploy")),
        ("description", vm_str("Deploy the app")),
        ("parameters", VmValue::Dict(Rc::new(deferred_params))),
        ("defer_loading", vm_bool(true)),
        ("namespace", vm_str("ops")),
    ]);
    let registry = vm_list(vec![deferred]);

    let openai = vm_tools_to_native(&registry, "openai").expect("openai native tools");
    assert_eq!(openai[0]["namespace"].as_str(), Some("ops"));
    assert_eq!(openai[0]["defer_loading"].as_bool(), Some(true));

    let anthropic = vm_tools_to_native(&registry, "anthropic").expect("anthropic native tools");
    assert_eq!(
        anthropic[0]["namespace"].as_str(),
        Some("ops"),
        "namespace survives Anthropic passthrough (harmlessly ignored by API)"
    );
}

#[test]
fn vm_tools_to_native_emits_defer_loading_for_openai_compat() {
    let registry = defer_loading_registry();
    let tools = vm_tools_to_native(&registry, "openai").expect("openai native tools");
    let deploy = tools
        .iter()
        .find(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                == Some("deploy")
        })
        .expect("deploy tool present");
    assert_eq!(
        deploy.get("defer_loading").and_then(|v| v.as_bool()),
        Some(true)
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
    apply_tool_search_native_injection(&mut tools, "anthropic", "bm25");
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
    apply_tool_search_native_injection(&mut tools, "anthropic", "regex");
    let tools = tools.unwrap();
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_regex_20251119")
    );
    assert_eq!(tools[0]["name"].as_str(), Some("tool_search_tool_regex"));
}

#[test]
fn apply_tool_search_native_injection_emits_openai_shape_for_non_anthropic() {
    let mut tools: Option<Vec<serde_json::Value>> = Some(vec![json!({"name": "look"})]);
    apply_tool_search_native_injection(&mut tools, "openai", "bm25");
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
    apply_tool_search_native_injection(&mut tools, "openai", "bm25");
    let tools = tools.unwrap();
    let namespaces = tools[0]["namespaces"]
        .as_array()
        .expect("namespaces present");
    let names: Vec<&str> = namespaces.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, vec!["crm", "ops"], "sorted + deduped");
}

#[test]
fn apply_tool_search_native_injection_creates_list_when_empty() {
    let mut tools: Option<Vec<serde_json::Value>> = None;
    apply_tool_search_native_injection(&mut tools, "anthropic", "bm25");
    let tools = tools.expect("tools populated");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0]["type"].as_str(),
        Some("tool_search_tool_bm25_20251119")
    );
}
