use serde_json::{json, Value as JsonValue};

use super::{ToolAudience, ToolCatalog};

fn tool(name: &str, command: &[&str], audiences: &[&str]) -> JsonValue {
    json!({
        "name": name,
        "inputSchema": {"type": "object"},
        "governance": {"audiences": audiences},
        "cli": {"command": command, "hidden": false},
        "deferLoading": false,
    })
}

fn catalog(tools: Vec<JsonValue>) -> JsonValue {
    json!({"schema_version": "harn-tools/1.0", "tools": tools})
}

#[test]
fn cli_projection_rejects_duplicate_and_parent_paths_in_either_order() {
    for tools in [
        vec![
            tool("first", &["inspect"], &["cli"]),
            tool("second", &["inspect"], &["cli"]),
        ],
        vec![
            tool("parent", &["inspect"], &["cli"]),
            tool("child", &["inspect", "run"], &["cli"]),
        ],
        vec![
            tool("child", &["inspect", "run"], &["cli"]),
            tool("parent", &["inspect"], &["cli"]),
        ],
    ] {
        assert!(
            serde_json::from_value::<ToolCatalog>(catalog(tools)).is_err(),
            "an ambiguous CLI command tree must fail at the typed contract boundary"
        );
    }
}

#[test]
fn adapter_specific_paths_and_mcp_projection_respect_governance() {
    let catalog: ToolCatalog = serde_json::from_value(catalog(vec![
        tool("cli_visible", &["inspect"], &["cli", "mcp"]),
        tool("dashboard_only", &["inspect"], &["dashboard"]),
    ]))
    .expect("a non-CLI tool may reuse a CLI presentation path");

    assert!(
        catalog
            .tool_for_audience("dashboard_only", ToolAudience::Mcp)
            .is_none(),
        "MCP admission must use the same governance projection as discovery"
    );
    let projected = catalog.mcp_tools().expect("MCP projection");
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0]["name"], "cli_visible");
}

#[test]
fn percent_encoded_component_names_validate_and_bundle_for_mcp() {
    let mut value = catalog(vec![tool("inspect", &["inspect"], &["mcp"])]);
    value["components"] = json!({
        "schemas": {"My Thing": {"type": "object", "properties": {"ok": {"type": "boolean"}}}}
    });
    value["tools"][0]["outputSchema"] = json!({"$ref": "#/components/schemas/My%20Thing"});

    let catalog: ToolCatalog = serde_json::from_value(value)
        .expect("URI-fragment percent encoding is part of Draft 2020-12 reference resolution");
    let projected = catalog.mcp_tool(&catalog.tools[0]).expect("MCP projection");
    assert_eq!(projected["outputSchema"]["$ref"], "#/$defs/My%20Thing");
    assert_eq!(
        projected["outputSchema"]["$defs"]["My Thing"]["type"],
        "object"
    );
    jsonschema::draft202012::new(&projected["outputSchema"])
        .expect("standalone projected schema resolves the encoded component");
}
