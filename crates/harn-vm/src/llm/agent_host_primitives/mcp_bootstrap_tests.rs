use super::tag_mcp_tool;
use crate::NativeVmExt;

#[test]
fn connected_runtime_metadata_survives_the_catalog_and_typed_harn_projection() {
    for measured in [true, false] {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let runtime = serde_json::to_value(crate::runtime_content_fingerprint()).unwrap();
        let mut tool = sample_tools().remove(0);
        if measured {
            let mut meta = serde_json::Map::new();
            meta.insert(
                crate::runtime_content::MCP_RUNTIME_CONTENT_META_KEY.into(),
                runtime.clone(),
            );
            meta.insert(
                "vendor.example/detail".into(),
                serde_json::json!("preserved"),
            );
            tool["_meta"] = serde_json::Value::Object(meta);
        }
        let tagged = tag_mcp_tool(tool.clone(), "project", true);
        if measured {
            assert_eq!(tagged["meta"], tool["_meta"]);
        }
        let registry = crate::schema::json_to_vm_value(&serde_json::json!({
            "_type": "tool_registry", "tools": [tagged],
        }));
        let catalog = crate::tool_registry::tool_registry_catalog(&registry).unwrap();
        let encoded =
            serde_json::to_string(&serde_json::to_string(&catalog.tools[0]).unwrap()).unwrap();
        let source = format!(
            r#"
            import {{ mcp_runtime_content_fingerprint }} from "std/runtime/content_fingerprint"
            import {{ ToolCatalogEntry }} from "std/tools"
            fn main(harness: Harness) {{
              const tool: ToolCatalogEntry = schema_expect(json_parse({encoded}), schema_of(ToolCatalogEntry))
              return mcp_runtime_content_fingerprint(tool)
            }}
        "#
        );
        let program = crate::compile_source(&source).unwrap();
        let result = crate::test_support::current_thread_runtime().block_on(
            harn_clock::test_support::with_local_tasks(async { vm.execute(&program).await }),
        );
        if measured {
            assert_eq!(
                crate::llm::helpers::vm_value_to_json(&result.unwrap()),
                runtime
            );
        } else {
            assert!(
                result.is_err(),
                "an unreported server identity must fail validation"
            );
        }
    }
}

fn sample_tools() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "search_issues",
            "description": "Search issues by query",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
            },
        }),
        serde_json::json!({
            "name": "create_issue",
            "description": "Create a new issue",
            "inputSchema": {
                "type": "object",
                "properties": { "title": { "type": "string" } },
            },
        }),
    ]
}

#[test]
fn catalog_defers_schemas_by_default() {
    let tools: Vec<serde_json::Value> = sample_tools()
        .into_iter()
        .map(|tool| tag_mcp_tool(tool, "github", false))
        .collect();
    assert_eq!(tools.len(), 2);
    for tool in &tools {
        // Catalog surfaces name + one-line description...
        assert!(tool.get("name").and_then(|v| v.as_str()).is_some());
        assert!(tool.get("description").and_then(|v| v.as_str()).is_some());
        // ...and defers the full input schema until tool_search /
        // dispatch reaches for it.
        assert_eq!(
            tool.get("defer_loading").and_then(|v| v.as_bool()),
            Some(true),
            "MCP tools should defer their schema by default"
        );
    }
    // Names are server-namespaced so cross-server collisions can't
    // happen, and the MCP executor wiring is preserved so the tool
    // stays callable once its schema is loaded on demand.
    let first = &tools[0];
    assert_eq!(
        first.get("name").and_then(|v| v.as_str()),
        Some("github__search_issues")
    );
    assert_eq!(
        first.get("executor").and_then(|v| v.as_str()),
        Some("mcp_server")
    );
    assert_eq!(
        first.get("mcp_server").and_then(|v| v.as_str()),
        Some("github")
    );
    assert_eq!(
        first.get("_mcp_server").and_then(|v| v.as_str()),
        Some("github")
    );
    assert_eq!(
        first.get("_mcp_tool_name").and_then(|v| v.as_str()),
        Some("search_issues")
    );
    // The full schema is still carried on the descriptor (it is held
    // back at the provider/agent-loop layer, not discarded), so it
    // resolves on demand when the tool is surfaced or called.
    assert!(first
        .get("inputSchema")
        .and_then(|v| v.as_object())
        .is_some());
}

#[test]
fn eager_opt_out_ships_schemas_upfront() {
    let tools: Vec<serde_json::Value> = sample_tools()
        .into_iter()
        .map(|tool| tag_mcp_tool(tool, "github", true))
        .collect();
    for tool in &tools {
        assert!(
            tool.get("defer_loading").is_none(),
            "eager_schemas: true must not defer MCP tool schemas"
        );
        assert!(tool
            .get("inputSchema")
            .and_then(|v| v.as_object())
            .is_some());
    }
}

#[test]
fn server_advertised_defer_loading_is_preserved() {
    // A server that explicitly sets defer_loading: false keeps it,
    // even under the progressive-disclosure default.
    let tool = serde_json::json!({
        "name": "ping",
        "description": "Health check",
        "defer_loading": false,
    });
    let tagged = tag_mcp_tool(tool, "ops", false);
    assert_eq!(
        tagged.get("defer_loading").and_then(|v| v.as_bool()),
        Some(false)
    );
}
