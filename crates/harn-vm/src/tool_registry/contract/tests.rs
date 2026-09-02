use super::*;

fn valid_catalog_json() -> JsonValue {
    json!({
      "schema_version": "harn-tools/2.0",
      "info": {
        "name": "inspection-suite",
        "version": "1.2.3",
        "description": "Typed inspection tools."
      },
      "tools": [{
        "name": "inspect",
        "title": "Inspect",
        "description": "Inspect one hypothesis.",
        "inputSchema": {"type": "object"},
        "outputSchema": {"$ref": "#/components/schemas/Result"},
        "governance": {"audiences": ["cli", "mcp", "catalog", "dashboard", "agent"]},
        "cli": {"command": ["inspect"], "hidden": false}, "deferLoading": false,
        "annotations": {
          "title": "Inspect a hypothesis",
          "readOnlyHint": true,
          "destructiveHint": false,
          "idempotentHint": true,
          "openWorldHint": false
        },
        "icons": [{"src": "data:image/svg+xml;base64,AA==", "mimeType": "image/svg+xml", "sizes": ["16x16"], "theme": "dark"}],
        "execution": {"taskSupport": "optional"},
        "namespace": "hypotheses",
        "policy": {"kind": "read", "side_effect_level": "read_only"},
        "source": {"kind": "harn", "id": "inspect", "binding": {"callableKind": "function"}},
        "_meta": {"com.example.vendor/version": 1}
      }],
      "components": {"schemas": {"Result": {"type": "object"}}}
    })
}

fn shared_invalid_catalogs() -> Vec<JsonValue> {
    let mut fixtures = Vec::new();
    let mut empty_name = valid_catalog_json();
    empty_name["tools"][0]["name"] = json!("");
    fixtures.push(empty_name);
    let mut scalar_input = valid_catalog_json();
    scalar_input["tools"][0]["inputSchema"] = json!(true);
    fixtures.push(scalar_input);
    let mut untyped_input = valid_catalog_json();
    untyped_input["tools"][0]["inputSchema"] = json!({});
    fixtures.push(untyped_input);
    let mut duplicate_audience = valid_catalog_json();
    duplicate_audience["tools"][0]["governance"]["audiences"] = json!(["mcp", "mcp"]);
    fixtures.push(duplicate_audience);
    let mut duplicate_size = valid_catalog_json();
    duplicate_size["tools"][0]["icons"][0]["sizes"] = json!(["16x16", "16x16"]);
    fixtures.push(duplicate_size);
    let mut scalar_component = valid_catalog_json();
    scalar_component["components"]["schemas"]["Result"] = json!(7);
    fixtures.push(scalar_component);
    let mut whitespace_info = valid_catalog_json();
    whitespace_info["info"]["name"] = json!("   ");
    fixtures.push(whitespace_info);
    let mut whitespace_icon = valid_catalog_json();
    whitespace_icon["tools"][0]["icons"][0]["src"] = json!("\t");
    fixtures.push(whitespace_icon);
    let mut whitespace_component = valid_catalog_json();
    let schema = whitespace_component["components"]["schemas"]
        .as_object_mut()
        .unwrap()
        .remove("Result")
        .unwrap();
    whitespace_component["components"]["schemas"][" "] = schema;
    whitespace_component["tools"][0]["outputSchema"] = json!({"$ref": "#/components/schemas/%20"});
    fixtures.push(whitespace_component);
    fixtures
}

#[test]
fn schema_is_meta_valid_and_rejects_owned_shape_drift() {
    let schema = tool_catalog_json_schema();
    jsonschema::meta::validate(&schema).expect("harn-tools schema must be meta-valid");
    let validator = jsonschema::draft202012::new(&schema).expect("catalog schema");
    let valid = valid_catalog_json();
    assert!(validator.is_valid(&valid));
    assert!(serde_json::from_value::<ToolCatalog>(valid).is_ok());

    let mut omitted_output = valid_catalog_json();
    omitted_output["tools"][0]
        .as_object_mut()
        .unwrap()
        .remove("outputSchema");
    assert!(
        validator.is_valid(&omitted_output),
        "outputSchema is optional in the generated envelope schema"
    );
    assert!(serde_json::from_value::<ToolCatalog>(omitted_output).is_ok());

    let mut repeated_command = valid_catalog_json();
    repeated_command["tools"][0]["cli"]["command"] = json!(["inspect", "inspect"]);
    assert!(validator.is_valid(&repeated_command));
    assert!(serde_json::from_value::<ToolCatalog>(repeated_command).is_ok());

    let empty = json!({"schema_version": "harn-tools/2.0", "tools": []});
    assert!(validator.is_valid(&empty));
    assert!(serde_json::from_value::<ToolCatalog>(empty).is_ok());

    for invalid in shared_invalid_catalogs().into_iter().chain([
            json!({"schema_version": "harn-tools/2.0", "tools": [], "unknown": true}),
            json!({"schema_version": "harn-tools/2.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["shell"]},"cli":{"command":["x"],"hidden":false},"deferLoading":false}]}),
            json!({"schema_version": "harn-tools/2.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["x"],"hidden":false},"deferLoading":false,"icons":[{"src":7}]}]}),
            json!({"schema_version": "harn-tools/2.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["x"],"hidden":false},"deferLoading":false,"execution":{"taskSupport":"sometimes"}}]}),
            json!({"schema_version": "harn-tools/2.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["-x"],"hidden":false},"deferLoading":false}]}),
            json!({"schema_version": "harn-tools/2.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["x y"],"hidden":false},"deferLoading":false}]}),
            json!({"schema_version": "harn-tools/2.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":[],"hidden":false},"deferLoading":false}]}),
        ]) {
            assert!(!validator.is_valid(&invalid), "fixture unexpectedly valid: {invalid}");
            assert!(
                serde_json::from_value::<ToolCatalog>(invalid.clone()).is_err(),
                "Rust parser unexpectedly accepted: {invalid}"
            );
        }
}

#[test]
fn structural_schema_defers_embedded_schema_semantics_to_harn() {
    let schema = tool_catalog_json_schema();
    let validator = jsonschema::draft202012::new(&schema).expect("catalog schema");
    let mut invalid_embedded_schema = valid_catalog_json();
    invalid_embedded_schema["tools"][0]["inputSchema"] = json!({"type": "object", "not": 7});
    assert!(
        validator.is_valid(&invalid_embedded_schema),
        "the portable artifact validates the envelope, not nested Draft semantics"
    );
    let error = serde_json::from_value::<ToolCatalog>(invalid_embedded_schema)
        .expect_err("Harn semantic validation must reject invalid embedded schemas");
    assert!(
        error
            .to_string()
            .contains("inputSchema is not a valid Draft 2020-12 JSON Schema"),
        "unexpected semantic validation error: {error}"
    );
}

#[test]
fn semantic_validation_rejects_duplicate_names_paths_and_dangling_refs() {
    let mut duplicate_names = valid_catalog_json();
    let mut second = duplicate_names["tools"][0].clone();
    second["cli"]["command"] = json!(["other"]);
    duplicate_names["tools"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert!(serde_json::from_value::<ToolCatalog>(duplicate_names).is_err());

    let mut duplicate_paths = valid_catalog_json();
    let mut second = duplicate_paths["tools"][0].clone();
    second["name"] = json!("other");
    duplicate_paths["tools"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert!(serde_json::from_value::<ToolCatalog>(duplicate_paths).is_err());

    let mut dangling = valid_catalog_json();
    dangling["tools"][0]["outputSchema"] = json!({"$ref": "#/components/schemas/Missing"});
    assert!(serde_json::from_value::<ToolCatalog>(dangling).is_err());

    let mut external = valid_catalog_json();
    external["tools"][0]["outputSchema"] = json!({"$ref": "https://example.test/x"});
    let external: ToolCatalog =
        serde_json::from_value(external).expect("portable catalogs retain open JSON Schema");
    assert!(external.mcp_tool(&external.tools[0]).is_err());
}

#[test]
fn semantic_validation_uses_draft_resource_and_anchor_resolution() {
    let mut anchored = valid_catalog_json();
    anchored["tools"][0]["outputSchema"] = json!({
        "$defs": {"result": {"$anchor": "node", "type": "string"}},
        "$ref": "#node"
    });
    serde_json::from_value::<ToolCatalog>(anchored)
        .expect("a local anchor is a valid Draft 2020-12 reference target");

    let mut dynamic_anchor = valid_catalog_json();
    dynamic_anchor["tools"][0]["outputSchema"] = json!({
        "$defs": {"result": {"$dynamicAnchor": "node", "type": "string"}},
        "$dynamicRef": "#node"
    });
    serde_json::from_value::<ToolCatalog>(dynamic_anchor)
        .expect("a local dynamic anchor is a valid Draft 2020-12 reference target");

    let mut dynamic_component = valid_catalog_json();
    dynamic_component["tools"][0]["outputSchema"] =
        json!({"$dynamicRef": "#/components/schemas/Result"});
    serde_json::from_value::<ToolCatalog>(dynamic_component)
        .expect("a dynamic reference can target a catalog component");

    let mut nested_resource = valid_catalog_json();
    nested_resource["tools"][0]["outputSchema"] = json!({
        "$id": "https://example.test/root",
        "allOf": [{
            "$id": "nested",
            "$defs": {"result": {"type": "string"}},
            "$ref": "#/$defs/result"
        }]
    });
    serde_json::from_value::<ToolCatalog>(nested_resource)
        .expect("a local pointer resolves from its containing schema resource");

    let mut legacy_definitions = valid_catalog_json();
    legacy_definitions["tools"][0]["outputSchema"] = json!({
        "definitions": {
            "Legacy": {"$ref": "#/components/schemas/Result"}
        },
        "$ref": "#/definitions/Legacy"
    });
    let legacy_catalog: ToolCatalog = serde_json::from_value(legacy_definitions)
        .expect("Draft 2020-12 retains definitions as a schema-valued map");
    let legacy_projected = legacy_catalog
        .mcp_tool(&legacy_catalog.tools[0])
        .expect("legacy definitions receive the same standalone MCP projection");
    assert_eq!(
        legacy_projected["outputSchema"]["definitions"]["Legacy"]["$ref"],
        "#/$defs/Result"
    );
    assert_eq!(
        legacy_projected["outputSchema"]["$defs"]["Result"]["type"],
        "object"
    );

    let mut dangling_anchor = valid_catalog_json();
    dangling_anchor["tools"][0]["outputSchema"] = json!({"$ref": "#missing"});
    assert!(serde_json::from_value::<ToolCatalog>(dangling_anchor).is_err());

    let mut dangling_dynamic_anchor = valid_catalog_json();
    dangling_dynamic_anchor["tools"][0]["outputSchema"] = json!({"$dynamicRef": "#missing"});
    assert!(serde_json::from_value::<ToolCatalog>(dangling_dynamic_anchor).is_err());

    let mut dangling_dynamic_component = valid_catalog_json();
    dangling_dynamic_component["tools"][0]["outputSchema"] =
        json!({"$dynamicRef": "#/components/schemas/Missing"});
    assert!(serde_json::from_value::<ToolCatalog>(dangling_dynamic_component).is_err());
}

#[test]
fn schema_projection_preserves_reference_shaped_instance_data() {
    let literal = json!({
        "$id": "literal-id",
        "$ref": "#/components/schemas/Missing",
        "$dynamicRef": "https://example.test/missing"
    });
    let mut value = valid_catalog_json();
    value["tools"][0]["outputSchema"] = json!({
        "type": "object",
        "properties": {
            "constant": {"const": &literal},
            "choice": {"enum": [&literal]},
            "result": {"$ref": "#/components/schemas/Result"}
        }
    });
    let catalog: ToolCatalog = serde_json::from_value(value)
        .expect("reference-shaped instance data is not a schema reference");
    let projected = catalog
        .mcp_tool(&catalog.tools[0])
        .expect("instance data does not make MCP projection unsafe");
    assert_eq!(
        projected["outputSchema"]["properties"]["constant"]["const"],
        literal
    );
    assert_eq!(
        projected["outputSchema"]["properties"]["choice"]["enum"][0],
        literal
    );
    assert_eq!(
        projected["outputSchema"]["$defs"]["Result"]["type"],
        "object"
    );
    assert!(projected["outputSchema"]["$defs"].get("Missing").is_none());
}

#[test]
fn explicit_null_optionals_are_accepted_and_omitted_when_reserialized() {
    let schema = tool_catalog_json_schema();
    let validator = jsonschema::draft202012::new(&schema).expect("catalog schema");
    let mut value = valid_catalog_json();
    value["info"]["version"] = JsonValue::Null;
    value["info"]["description"] = JsonValue::Null;
    for field in [
        "title",
        "description",
        "outputSchema",
        "annotations",
        "icons",
        "execution",
        "namespace",
        "source",
        "policy",
        "_meta",
    ] {
        value["tools"][0][field] = JsonValue::Null;
    }
    value["components"] = JsonValue::Null;
    assert!(
        validator.is_valid(&value),
        "schema must accept explicit nulls"
    );
    let catalog: ToolCatalog = serde_json::from_value(value).expect("Rust parser accepts nulls");
    let wire = serde_json::to_value(catalog).expect("catalog serializes");
    assert!(wire["info"].get("version").is_none());
    assert!(wire["tools"][0].get("outputSchema").is_none());
    assert!(wire.get("components").is_none());
}

#[test]
fn mcp_projection_fails_closed_for_unsupported_resource_scope() {
    for (keyword, keyword_value) in [
        ("$id", json!("nested")),
        ("$anchor", json!("node")),
        ("$dynamicAnchor", json!("node")),
    ] {
        let mut value = valid_catalog_json();
        value["components"]["schemas"]["Result"][keyword] = keyword_value;
        let catalog: ToolCatalog = serde_json::from_value(value)
            .unwrap_or_else(|error| panic!("catalog must retain {keyword}: {error}"));
        let error = catalog
            .mcp_tool(&catalog.tools[0])
            .expect_err("resource-scoped component cannot be safely embedded");
        assert!(error.to_string().contains(keyword));
    }

    for (keyword, keyword_value) in [
        ("$dynamicRef", json!("https://example.test/schema#node")),
        ("$ref", json!("https://example.test/schema")),
    ] {
        let mut value = valid_catalog_json();
        value["tools"][0]["outputSchema"] = json!({(keyword): keyword_value});
        let catalog: ToolCatalog = serde_json::from_value(value)
            .unwrap_or_else(|error| panic!("catalog must retain {keyword}: {error}"));
        let error = catalog
            .mcp_tool(&catalog.tools[0])
            .expect_err("non-standalone entry schema must fail MCP preparation");
        assert!(error.to_string().contains(keyword));
    }
}

#[test]
fn mcp_projection_prefers_annotation_title_and_bundles_components() {
    let mut value = valid_catalog_json();
    value["tools"][0]["title"] = json!("General catalog title");
    value["tools"][0]["annotations"]["title"] = json!("Human title");
    value["tools"][0]["outputSchema"] = json!({"$ref": "#/components/schemas/Result~1Envelope"});
    value["components"]["schemas"]["Result/Envelope"] = json!({
        "type": "object",
        "$defs": {"Label": {"type": "string"}},
        "properties": {
            "label": {"$ref": "#/$defs/Label"},
            "detail": {"$ref": "#/components/schemas/Detail"},
            "child": {"$ref": "#"}
        },
        "required": ["label", "detail"]
    });
    value["components"]["schemas"]["Detail"] = json!({"type": "string"});
    value["components"]["schemas"]["Unused"] = json!({"type": "integer"});
    let catalog: ToolCatalog = serde_json::from_value(value).expect("valid catalog");
    let projected = catalog.mcp_tool(&catalog.tools[0]).expect("MCP projection");
    assert_eq!(projected["title"], "General catalog title");
    assert_eq!(projected["annotations"]["title"], "Human title");
    assert_eq!(
        projected["outputSchema"]["$ref"],
        "#/$defs/Result~1Envelope"
    );
    assert_eq!(
        projected["outputSchema"]["$defs"]["Result/Envelope"]["type"],
        "object"
    );
    assert_eq!(
        projected["outputSchema"]["$defs"]["Result/Envelope"]["properties"]["detail"]["$ref"],
        "#/$defs/Detail"
    );
    assert_eq!(
        projected["outputSchema"]["$defs"]["Result/Envelope"]["properties"]["label"]["$ref"],
        "#/$defs/Result~1Envelope/$defs/Label"
    );
    assert_eq!(
        projected["outputSchema"]["$defs"]["Result/Envelope"]["properties"]["child"]["$ref"],
        "#/$defs/Result~1Envelope"
    );
    assert_eq!(
        projected["outputSchema"]["$defs"]["Detail"]["type"],
        "string"
    );
    assert!(projected["outputSchema"]["$defs"].get("Unused").is_none());
    assert!(projected["outputSchema"]["$defs"].get("Result").is_none());

    let structured = catalog
        .mcp_structured_content(
            &catalog.tools[0],
            json!({
                "label": "root",
                "detail": "visible",
                "child": {"label": "nested", "detail": "visible"}
            }),
        )
        .expect("MCP result projection")
        .expect("declared output");
    let validator = jsonschema::draft202012::new(&projected["outputSchema"])
        .expect("standalone schema resolves every rebased ref");
    assert!(validator.is_valid(&structured));
}

#[test]
fn wire_preserves_optional_and_false_compatibility() {
    let catalog: ToolCatalog = serde_json::from_value(valid_catalog_json()).unwrap();
    let wire = serde_json::to_value(&catalog).unwrap();
    assert_eq!(wire["tools"][0]["cli"]["hidden"], false);
    assert_eq!(wire["tools"][0]["deferLoading"], false);
    assert_eq!(wire["tools"][0]["icons"][0]["theme"], "dark");
}
