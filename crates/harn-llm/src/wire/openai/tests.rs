use super::{encode_request, test_support::base_request_payload, OpenAiWireContext};
use crate::request::{OutputFormat, ThinkingConfig};
use harn_provider_catalog::capabilities::{Capabilities, ReasoningEffort};
use serde_json::json;

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
