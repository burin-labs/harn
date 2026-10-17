use super::*;
use serde_json::json;

fn normalize(verify: Value, file: Option<&Value>) -> Result<Option<VerificationContract>, VmError> {
    verification_contract_from_sources("verify", verify.as_object().unwrap(), file)
}

#[test]
fn sources_preserve_precedence_and_merge_unique_exact_requirements() {
    let file = json!({
        "command": " file verifier ", "expect_status": 0,
        "identifiers": [" rateLimit ", "rateLimit"],
        "checks": [{"kind": " custom ", "value": " exact ", "description": " reason "}]
    });
    let contract = normalize(json!({
        "contract": {"command": "inline verifier", "expect_status": 1,
                     "required_identifiers": ["rateLimit", "other"], "paths": " src/main.harn "},
        "command": "outer verifier", "expect_status": 2,
        "required_strings": [" exact text ", "exact text"],
        "checks": [{"kind": "custom", "value": "exact", "note": "reason"}]
    }), Some(&file)).unwrap().unwrap();
    assert_eq!(contract.source_node.as_deref(), Some("verify"));
    assert_eq!(contract.command.as_deref(), Some("file verifier"));
    assert_eq!(contract.expect_status, Some(0));
    assert_eq!(contract.required_identifiers, ["rateLimit", "other"]);
    assert_eq!(contract.required_paths, ["src/main.harn"]);
    assert_eq!(contract.required_text, ["exact text"]);
    assert_eq!(
        contract
            .checks
            .iter()
            .filter(|check| check.kind == "custom")
            .count(),
        1
    );
    assert!(contract
        .checks
        .iter()
        .any(|check| check.kind == "expect_status" && check.value == "0"));
    assert!(contract
        .checks
        .iter()
        .any(|check| check.kind == "identifier" && check.value == "other"));
    assert!(contract
        .checks
        .iter()
        .any(|check| check.kind == "text" && check.value == "exact text"));
}

#[test]
fn empty_or_malformed_requirements_do_not_create_a_contract() {
    assert_eq!(
        normalize(
            json!({"notes": [" ", 4], "checks": [null, {"kind": "x"}]}),
            None
        )
        .unwrap(),
        None
    );
    let zero = normalize(json!({"expect_status": 0}), None)
        .unwrap()
        .unwrap();
    assert_eq!(zero.expect_status, Some(0));
}

#[test]
fn malformed_file_and_inline_contracts_fail_at_the_owning_boundary() {
    assert!(
        matches!(normalize(json!({}), Some(&json!([]))), Err(VmError::Runtime(message))
        if message == "workflow verification contract file must parse to a JSON object")
    );
    assert!(
        matches!(normalize(json!({"contract": []}), None), Err(VmError::Runtime(message))
        if message == "workflow verify.contract must be an object")
    );
}

#[test]
fn visible_and_command_output_requirements_remain_distinct() {
    let contract = normalize(
        json!({"assert_text": " visible ", "expect_text": " command "}),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(contract
        .checks
        .iter()
        .any(|check| check.kind == "visible_text_contains" && check.value == "visible"));
    assert!(contract
        .checks
        .iter()
        .any(|check| check.kind == "combined_output_contains" && check.value == "command"));
    assert!(!contract
        .checks
        .iter()
        .any(|check| check.kind == "visible_text_contains" && check.value == "command"));
}
