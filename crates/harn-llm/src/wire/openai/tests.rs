use super::{encode_request, test_support::base_request_payload, OpenAiWireContext};
use crate::request::{OutputFormat, ThinkingConfig};
use harn_provider_catalog::capabilities::{Capabilities, ReasoningEffort};
use serde_json::json;

#[test]
fn native_numeric_const_enum_intersection_preserves_response_contract() {
    for (constant, values, accepted, refused) in [
        (json!(1), json!([1, 2]), json!(1), json!(2)),
        (json!(1), json!([1.0, 2]), json!(1.0), json!(2)),
        (json!(1.0), json!([1, 2]), json!(1), json!(2)),
        (
            json!(9007199254740993_u64),
            json!([9007199254740992_u64, 9007199254740993_u64]),
            json!(9007199254740993_u64),
            json!(9007199254740992_u64),
        ),
    ] {
        let schema = json!({
            "type": "object", "additionalProperties": false,
            "properties": {"value": {"type": "number", "const": constant, "enum": values}},
            "required": ["value"]
        });
        let mut request = base_request_payload();
        request.provider = "openai".into();
        request.output_format = OutputFormat::JsonSchema {
            schema: schema.clone(),
            strict: true,
        };
        let caps = Capabilities::default();
        let context = OpenAiWireContext {
            capabilities: &caps,
            wire_model: "native-selector",
            may_shape_reasoning: true,
            tool_search_extensions: false,
            fast_request: None,
        };
        let encoded = encode_request(&request, &context);
        let sent = &encoded.body["response_format"]["json_schema"]["schema"];
        assert_eq!(sent["properties"]["value"]["enum"], json!([constant]));
        let effective = crate::schema_compat::project_output_schema(
            "openai",
            &request.model,
            harn_provider_catalog::capabilities::WireDialect::OpenAiCompat,
            true,
            &schema,
        );
        assert_eq!(sent, &effective.schema);
        let validator = jsonschema::draft202012::new(&effective.schema).unwrap();
        assert!(validator.is_valid(&json!({"value": accepted})));
        assert!(!validator.is_valid(&json!({"value": refused})));
    }
}

#[test]
fn captured_policy_controls_encoding_and_returns_audit_records() {
    let mut request = base_request_payload();
    request.provider = "openai".into();
    request.model = "native-selector".into();
    request.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::None,
    };
    request.output_format = OutputFormat::JsonSchema {
        schema: json!({"type": "string", "default": "removed"}),
        strict: true,
    };
    let mut caps = Capabilities::default();
    caps.reasoning_effort_supported = false;
    caps.reasoning_none_supported = false;
    let mut context = OpenAiWireContext {
        capabilities: &caps,
        wire_model: "resolved-wire-id",
        may_shape_reasoning: true,
        tool_search_extensions: false,
        fast_request: None,
    };
    let ordinary = encode_request(&request, &context);
    assert_eq!(ordinary.body["model"], "resolved-wire-id");
    assert!(ordinary.body.get("reasoning_effort").is_none());
    assert!(ordinary.body["response_format"]["json_schema"]["schema"]
        .get("default")
        .is_none());
    assert_eq!(ordinary.schema_audits.len(), 1);
    assert_eq!(ordinary.schema_audits[0]["provider"], "openai");
    assert_eq!(ordinary.schema_audits[0]["model"], "native-selector");
    assert!(ordinary.schema_audits[0]["changes"]
        .as_array()
        .is_some_and(|changes| !changes.is_empty()));
    assert_eq!(
        request.output_format.schema().unwrap()["default"],
        "removed"
    );

    // The same payload must send the probed setting when shaping is suspended.
    context.may_shape_reasoning = false;
    let probe = encode_request(&request, &context);
    assert_eq!(probe.body["reasoning_effort"], "none");
    assert_eq!(probe.schema_audits, ordinary.schema_audits);
}
