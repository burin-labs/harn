use super::tag_mcp_tool;

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
