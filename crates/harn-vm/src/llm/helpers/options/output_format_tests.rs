use super::output::*;
use super::*;
use crate::value::VmDictExt;

#[test]
fn parses_explicit_json_schema_output_format() {
    let mut fmt = crate::value::DictMap::new();
    fmt.put_str("kind", "json_schema");
    fmt.insert(
        crate::value::intern_key("schema"),
        VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("object")),
        )])),
    );
    fmt.insert(crate::value::intern_key("strict"), VmValue::Bool(false));
    let options = crate::value::DictMap::from_iter([(
        crate::value::intern_key("output_format"),
        VmValue::dict(fmt),
    )]);

    let parsed = parse_output_format_option(Some(&options), None, None).expect("output_format");

    assert_eq!(
        parsed,
        crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "object"}),
            strict: false,
        }
    );
}

#[test]
fn normalizes_harn_schema_types_for_provider_output() {
    let schema = crate::schema::json_to_vm_value(&serde_json::json!({
        "type": "dict",
        "properties": {
            "items": {
                "type": "list",
                "items": {"type": "int"},
                "x-provider-extension": {"mode": "strict"}
            }
        }
    }));

    let parsed = parse_schema_value(Some(&schema), "output_schema")
        .expect("valid Harn schema")
        .expect("present schema");

    assert_eq!(parsed["type"], "object");
    assert_eq!(parsed["properties"]["items"]["type"], "array");
    assert_eq!(parsed["properties"]["items"]["items"]["type"], "integer");
    assert_eq!(
        parsed["properties"]["items"]["x-provider-extension"]["mode"],
        "strict"
    );
}

#[test]
fn legacy_response_format_and_json_schema_map_to_typed_output_format() {
    let schema = serde_json::json!({"type": "object"});

    let parsed = parse_output_format_option(
        Some(&crate::value::DictMap::new()),
        Some("json"),
        Some(&schema),
    )
    .expect("legacy output format");

    assert_eq!(
        parsed,
        crate::llm::api::OutputFormat::JsonSchema {
            schema,
            strict: true,
        }
    );
}

#[test]
fn rejects_json_schema_when_capability_is_absent() {
    crate::llm::capabilities::clear_user_overrides();
    let err = validate_output_format_supported(
        &crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        },
        "custom-provider",
        "custom-model",
        &crate::llm::capabilities::lookup("custom-provider", "custom-model"),
    )
    .expect_err("unsupported structured output should fail");

    assert!(err
        .to_string()
        .contains("option `output_format` is not supported by `custom-model`"));
}

#[test]
fn accepts_json_schema_when_capability_declares_strategy() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.custom-provider]]
model_match = "*"
structured_output = "format_kw"
"#,
    )
    .expect("capability override");

    validate_output_format_supported(
        &crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        },
        "custom-provider",
        "custom-model",
        &crate::llm::capabilities::lookup("custom-provider", "custom-model"),
    )
    .expect("supported structured output");
    crate::llm::capabilities::clear_user_overrides();
}
