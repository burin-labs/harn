//! Orchestration parse failures must not echo private payloads into diagnostics.

use serde::Deserialize;
use serde_json::json;

use crate::orchestration::parse_json_payload;

#[derive(Debug, Deserialize, PartialEq)]
struct Expected {
    required_field: String,
}

#[test]
fn multibyte_payload_reports_a_stable_typed_error() {
    for pad_chars in 400..460 {
        let error = parse_json_payload::<Expected>(
            json!({"unexpected": "æ".repeat(pad_chars)}),
            "orchestration",
        )
        .expect_err("payload is missing required_field");
        assert_eq!(
            error.to_string(),
            "Runtime error: orchestration parse error: payload does not match expected schema"
        );
    }
    assert_eq!(
        parse_json_payload::<Expected>(json!({"required_field": "valid"}), "orchestration")
            .expect("valid input must still decode"),
        Expected {
            required_field: "valid".to_string()
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_friction_parsers_do_not_echo_invalid_values_keys_or_adjacent_context() {
    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    for invalid in [
        json!({"estimated_time_ms": "invalid-value-private-canary"}),
        json!({"human_hypothesis": {"invalid-key-private-canary": true, "confidence": []}}),
    ] {
        let mut event = json!({
            "kind": "missing_context",
            "redacted_summary": "A valid summary",
            "metadata": {"api_key": "adjacent-credential-private-canary"},
            "raw_prompt": "adjacent-prompt-private-canary",
        });
        event
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        for builtin in ["context_pack_suggestions", "friction_eval_fixture"] {
            let payload = if builtin == "friction_eval_fixture" {
                json!({"events": [event.clone()]})
            } else {
                json!([event.clone()])
            };
            let error = vm
                .call_named_builtin(builtin, vec![crate::json_to_vm_value(&payload)])
                .await
                .expect_err("invalid event must reach and fail the public parser");
            assert_eq!(
                error.to_string(),
                "Runtime error: friction_event parse error: payload does not match expected schema"
            );
        }
    }
    let events = json!([
        {"id": "first", "timestamp": "1", "kind": "missing_context", "redacted_summary": "Need the build instructions"},
        {"id": "second", "timestamp": "2", "kind": "missing_context", "redacted_summary": "Need the build instructions"},
    ]);
    let suggestions = vm
        .call_named_builtin(
            "context_pack_suggestions",
            vec![crate::json_to_vm_value(&events)],
        )
        .await
        .expect("valid events must still decode through the public parser");
    assert!(matches!(suggestions, crate::value::VmValue::List(ref list) if list.len() == 1));
    let fixture = vm
        .call_named_builtin(
            "friction_eval_fixture",
            vec![crate::json_to_vm_value(&json!({"events": events}))],
        )
        .await
        .expect("valid fixture must still decode through the public parser");
    let fixture = crate::llm::vm_value_to_json(&fixture);
    assert_eq!(fixture["event_count"], 2);
    assert_eq!(fixture["suggestion_count"], 1);
}

#[test]
fn invalid_dynamic_map_key_does_not_become_a_diagnostic_path() {
    let error = parse_json_payload::<std::collections::BTreeMap<String, bool>>(
        json!({"invalid-key-private-canary": "invalid-value-private-canary"}),
        "orchestration",
    )
    .expect_err("invalid map value must fail schema decoding");
    assert_eq!(
        error.to_string(),
        "Runtime error: orchestration parse error: payload does not match expected schema"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_manifest_source_parser_does_not_echo_json_or_toml_values_or_source() {
    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    for source in [
        r#"{"name":"Private context","owner":"founder","version":"invalid-json-private-canary","metadata":{"api_key":"adjacent-private-canary"}}"#,
        "name = \"Private context\"\nowner = \"founder\"\nversion = \"invalid-toml-private-canary\"\n[metadata]\napi_key = \"adjacent-private-canary\"",
        r#"{"name":"Private context","owner":"founder","syntax-private-canary":}"#,
        "name = \"Private context\"\nowner = \"founder\"\nsyntax-private-canary = { token = \"adjacent-private-canary\"",
    ] {
        let error = vm
            .call_named_builtin(
                "context_pack_manifest_parse",
                vec![crate::json_to_vm_value(&json!(source))],
            )
            .await
            .expect_err("invalid manifest must reach and fail the public source parser");
        assert_eq!(
            error.to_string(),
            "Runtime error: context_pack_manifest_parse parse error: payload does not match expected schema"
        );
    }
    for source in [
        r#"{"name":"Build context","owner":"founder"}"#,
        "name = \"Build context\"\nowner = \"founder\"",
    ] {
        let manifest = vm
            .call_named_builtin(
                "context_pack_manifest_parse",
                vec![crate::json_to_vm_value(&json!(source))],
            )
            .await
            .expect(
                "valid JSON and TOML must still parse and normalize through the public builtin",
            );
        let manifest = crate::llm::vm_value_to_json(&manifest);
        assert_eq!(manifest["name"], "Build context");
        assert_eq!(manifest["owner"], "founder");
        assert_eq!(manifest["version"], 1);
        assert_eq!(manifest["id"], "build_context");
    }
}
