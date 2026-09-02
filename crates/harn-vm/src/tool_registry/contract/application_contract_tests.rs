use super::*;

fn valid_catalog_json() -> JsonValue {
    json!({
        "schema_version": "harn-tools/2.0",
        "info": {"name": "inspection-suite"},
        "tools": [{
            "name": "inspect",
            "inputSchema": {"type": "object"},
            "outputSchema": {"$ref": "#/components/schemas/Result"},
            "governance": {"audiences": ["mcp"]},
            "cli": {"command": ["inspect"], "hidden": false},
            "deferLoading": false
        }],
        "components": {"schemas": {"Result": {"type": "object"}}}
    })
}

#[test]
fn catalog_rejects_schema_dialects_the_runtime_would_not_execute() {
    for phase in ["inputSchema", "outputSchema", "errorSchema"] {
        let mut value = valid_catalog_json();
        value["tools"][0][phase] = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object"
        });
        let error = serde_json::from_value::<ToolCatalog>(value)
            .expect_err("catalog must not validate one dialect and execute another");
        assert!(
            error.to_string().contains("expected Draft 2020-12"),
            "{error}"
        );
    }

    let mut component = valid_catalog_json();
    component["components"]["schemas"]["Result"] = json!({
        "$schema": "https://json-schema.org/draft/2019-09/schema",
        "type": "object"
    });
    assert!(serde_json::from_value::<ToolCatalog>(component)
        .expect_err("component dialect must match runtime semantics")
        .to_string()
        .contains("expected Draft 2020-12"));

    let mut current = valid_catalog_json();
    current["tools"][0]["outputSchema"]["$schema"] =
        json!("https://json-schema.org/draft/2020-12/schema#");
    serde_json::from_value::<ToolCatalog>(current)
        .expect("the canonical 2020-12 dialect remains accepted");
}

#[test]
fn catalog_rejects_nested_runtime_only_harn_schema_types() {
    for phase in ["inputSchema", "outputSchema", "errorSchema"] {
        let mut value = valid_catalog_json();
        value["tools"][0][phase] = json!({
            "type": "object",
            "properties": {
                "callback": {"type": "string", "x-harn-type": "closure"}
            }
        });
        let error = serde_json::from_value::<ToolCatalog>(value)
            .expect_err("runtime closure schemas cannot enter a portable catalog");
        assert!(
            error.to_string().contains(&format!(
                "{phase} contains runtime-only Harn type \"closure\""
            )),
            "{error}"
        );
    }
}

#[test]
fn catalog_rejects_invalid_or_reserved_mcp_metadata_keys() {
    for key in [
        "io.modelcontextprotocol/private",
        "com.mcp.vendor/private",
        "bad key",
    ] {
        let mut invalid = valid_catalog_json();
        invalid["tools"][0]["_meta"] =
            JsonValue::Object(serde_json::Map::from_iter([(key.to_string(), json!(true))]));
        let error = serde_json::from_value::<ToolCatalog>(invalid)
            .expect_err("invalid MCP metadata key must be rejected");
        assert!(error.to_string().contains("._meta key"), "{error}");
    }
}

#[test]
fn serde_rejects_unknown_owned_fields_and_versions() {
    assert!(serde_json::from_value::<ToolCatalog>(json!({
        "schema_version": "harn-tools/2.0", "tools": [], "extra": true
    }))
    .is_err());
    assert!(serde_json::from_value::<ToolCatalog>(json!({
        "schema_version": "harn-tools/1.0", "tools": []
    }))
    .is_err());
    assert!(serde_json::from_value::<ToolCatalog>(json!({
        "schema_version": "harn-tools/9.0", "tools": []
    }))
    .is_err());
}

#[test]
fn non_object_output_schema_and_result_preserve_the_canonical_value() {
    let mut value = valid_catalog_json();
    value["tools"][0]["outputSchema"] = json!({"type": "string"});
    let catalog: ToolCatalog = serde_json::from_value(value).expect("valid catalog");
    let entry = &catalog.tools[0];
    let projected = catalog.mcp_tool(entry).expect("MCP projection");
    let structured =
        tool_result_to_mcp_structured_content(entry, json!("ok")).expect("declared output");
    assert_eq!(projected["outputSchema"], json!({"type": "string"}));
    assert_eq!(structured, json!("ok"));
    let validator = jsonschema::draft202012::new(&projected["outputSchema"])
        .expect("valid standalone output schema");
    assert!(validator.is_valid(&structured));
}

#[test]
fn mcp_projects_declared_error_schema_under_reserved_metadata() {
    let mut value = valid_catalog_json();
    value["tools"][0]["errorSchema"] = json!({"$ref": "#/components/schemas/Result"});
    let catalog: ToolCatalog = serde_json::from_value(value).expect("valid catalog");
    let projected = catalog.mcp_tool(&catalog.tools[0]).expect("MCP projection");
    assert_eq!(
        projected["_meta"][HARN_MCP_TOOL_CONTRACT_META_KEY]["errorSchema"]["$ref"],
        "#/$defs/Result"
    );

    let mut reserved = valid_catalog_json();
    reserved["tools"][0]["_meta"] = json!({HARN_MCP_TOOL_CONTRACT_META_KEY: {"errorSchema": {}}});
    let error = serde_json::from_value::<ToolCatalog>(reserved)
        .expect_err("caller metadata cannot shadow Harn projection");
    assert!(
        error.to_string().contains(HARN_MCP_TOOL_CONTRACT_META_KEY),
        "{error}"
    );
}

#[test]
fn typescript_preserves_optional_and_false_compatibility() {
    let typescript = tool_catalog_typescript();
    assert!(typescript.ends_with('\n'));
    assert!(!typescript.ends_with("\n\n"));
    assert!(typescript.lines().all(|line| line == line.trim_end()));
    for expected in [
        "title?: string | null",
        "theme?: ToolIconTheme | null",
        "outputSchema?: JsonSchema202012 | null",
        "errorSchema?: JsonSchema202012 | null",
        "binding?: Readonly<Record<string, JsonValue>> | null",
        "_meta?: Readonly<Record<string, JsonValue>> | null",
        "hidden: boolean",
        "deferLoading: boolean",
    ] {
        assert!(
            typescript.contains(expected),
            "missing TS fragment {expected:?}"
        );
    }
}
