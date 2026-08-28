use super::output::*;
use super::*;

#[test]
fn parses_explicit_json_schema_output() {
    let mut spec = crate::value::DictMap::new();
    spec.insert(
        crate::value::intern_key("schema"),
        VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("object")),
        )])),
    );
    spec.insert(crate::value::intern_key("strict"), VmValue::Bool(false));
    let options = crate::value::DictMap::from_iter([(
        crate::value::intern_key("output"),
        VmValue::dict(spec),
    )]);

    let parsed = parse_output_option(Some(&options)).expect("output");

    assert_eq!(
        parsed.format,
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

    let parsed = parse_schema_value(Some(&schema), "output")
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
fn normalizes_harn_schema_types_in_unions_and_type_arrays() {
    // The completion-judge schema that regressed llama.cpp nests aliases in
    // positions the two happy-path tests do not cover: `anyOf` branches, a
    // union `type` array, and the `float`/`nil` scalar siblings. Pin them so a
    // future normalizer refactor cannot silently reopen the strict-grammar 400.
    let schema = crate::schema::json_to_vm_value(&serde_json::json!({
        "type": "dict",
        "properties": {
            "verdict": {"anyOf": [{"type": "bool"}, {"type": "nil"}]},
            "score": {"type": "float"},
            "tags": {"type": ["list", "nil"]}
        }
    }));

    let parsed = parse_schema_value(Some(&schema), "output")
        .expect("valid Harn schema")
        .expect("present schema");

    assert_eq!(parsed["type"], "object");
    assert_eq!(
        parsed["properties"]["verdict"]["anyOf"][0]["type"],
        "boolean"
    );
    assert_eq!(parsed["properties"]["verdict"]["anyOf"][1]["type"], "null");
    assert_eq!(parsed["properties"]["score"]["type"], "number");
    assert_eq!(
        parsed["properties"]["tags"]["type"],
        serde_json::json!(["array", "null"])
    );
}

#[test]
fn projects_harn_union_keyword_to_provider_json_schema() {
    let schema = crate::schema::json_to_vm_value(&serde_json::json!({
        "type": "dict",
        "properties": {
            "optional_label": {
                "union": [{"type": "string"}, {"type": "nil"}]
            }
        }
    }));

    let parsed = parse_schema_value(Some(&schema), "output")
        .expect("valid Harn schema")
        .expect("present schema");
    let optional = &parsed["properties"]["optional_label"];

    assert_eq!(
        optional["anyOf"],
        serde_json::json!([{"type": "string"}, {"type": "null"}])
    );
    assert!(
        optional.get("union").is_none(),
        "provider request schemas must not leak Harn's internal union keyword"
    );
}

#[test]
fn parser_does_not_revive_removed_output_synonyms() {
    // W2 collapsed the `response_format` / `json_schema` / top-level `schema`
    // spellings onto the single canonical `output` key. `parse_output_option`
    // no longer reads the legacy keys, so a call carrying only them lowers to
    // plain text output — guarding against an accidental synonym revival.
    let schema = crate::value::DictMap::from_iter([(
        crate::value::intern_key("type"),
        VmValue::String(arcstr::ArcStr::from("object")),
    )]);
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("response_format"),
            VmValue::String(arcstr::ArcStr::from("json")),
        ),
        (
            crate::value::intern_key("json_schema"),
            VmValue::dict(schema),
        ),
    ]);

    let parsed = parse_output_option(Some(&options)).expect("output");

    assert_eq!(parsed.format, crate::llm::api::OutputFormat::Text);
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
        .contains("option `output` is not supported by `custom-model`"));
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

#[test]
fn rejects_empty_any_of_after_union_rewrite() {
    let schema = crate::schema::json_to_vm_value(&serde_json::json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "enum": ["edit"]},
            "args": {
                "type": "object",
                "properties": {
                    "action": {
                        "anyOf": [],
                        "x-harn-collapsed-branches": ["create", "replace_range"]
                    }
                },
                "required": ["action"]
            }
        },
        "required": ["name", "args"]
    }));

    let error = parse_schema_value(Some(&schema), "output").expect_err("empty anyOf");
    let message = error.to_string();
    assert!(
        message.contains("tool `edit`"),
        "diagnostic must name the tool: {message}"
    );
    assert!(
        message.contains("collapsed branch set [create, replace_range]"),
        "diagnostic must name the collapsed branches: {message}"
    );
}

#[test]
fn constrained_request_for_discriminated_union_after_rejected_call() {
    use crate::llm::api::{LlmCallOptions, LlmRequestPayload, OutputFormat};
    use crate::llm::providers::schema_compat::project_output_schema_for_provider;

    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": {"type": "string", "enum": ["edit"]},
            "args": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "oneOf": [
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "action": {"const": "create"},
                                    "path": {"type": "string"}
                                },
                                "required": ["action", "path"]
                            },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "action": {"const": "replace_range"},
                                    "path": {"type": "string"}
                                },
                                "required": ["action", "path"]
                            }
                        ]
                    }
                },
                "required": ["action"]
            }
        },
        "required": ["name", "args"]
    });
    let mut opts = LlmCallOptions {
        provider: "llamacpp".to_string(),
        model: "local".to_string(),
        output_format: OutputFormat::JsonSchema {
            schema: schema.clone(),
            strict: true,
        },
        output_schema: Some(schema),
        ..LlmCallOptions::default()
    };
    let payload = LlmRequestPayload::from(&opts);
    let sent = payload
        .output_schema
        .as_ref()
        .expect("constrained request carries a schema");
    crate::schema::reject_unsatisfiable_output_schema(sent)
        .expect("rejected-call follow-up must not emit an empty schema");
    let projected = project_output_schema_for_provider("openai", "gpt-5.4", true, sent, false);
    let branches = projected
        .pointer("/properties/args/properties/action/anyOf")
        .or_else(|| projected.pointer("/properties/args/properties/action/oneOf"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    assert!(
        branches >= 1,
        "serialized schema must keep at least one admissible action branch: {projected}"
    );

    opts.output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {
            "name": {"enum": ["edit"]},
            "args": {
                "properties": {
                    "action": {
                        "anyOf": [],
                        "x-harn-collapsed-branches": ["create", "replace_range"]
                    }
                }
            }
        }
    }));
    opts.output_format = OutputFormat::JsonSchema {
        schema: opts.output_schema.clone().expect("collapsed schema"),
        strict: true,
    };
    let collapsed = LlmRequestPayload::from(&opts);
    let error = crate::schema::reject_unsatisfiable_output_schema(
        collapsed.output_schema.as_ref().expect("schema"),
    )
    .expect_err("empty collapsed union must be rejected before emit");
    assert_eq!(error.tool.as_deref(), Some("edit"));
    assert_eq!(
        error.collapsed_branches,
        ["create".to_string(), "replace_range".to_string()]
    );
}
