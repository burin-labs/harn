use super::execution_identity::run_record_impl;
use super::normalize_run_record;
use crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION;

fn constructed_run_json(payload: serde_json::Value) -> serde_json::Value {
    let value = run_record_impl(
        &[crate::stdlib::json_to_vm_value(&payload)],
        &mut String::new(),
    )
    .expect("construct run record");
    crate::llm::vm_value_to_json(&value)
}

#[test]
fn run_record_constructor_inherits_one_active_execution_identity() {
    let execution_id = crate::mint_execution_scope();
    let _scope = crate::observability::execution_scope::enter_execution_scope(execution_id.clone());

    let first = constructed_run_json(serde_json::json!({}));
    let second = constructed_run_json(serde_json::json!({}));

    assert_eq!(
        first["evidence"]["execution_id"].as_str(),
        Some(execution_id.as_ref())
    );
    assert_eq!(
        second["evidence"]["execution_id"].as_str(),
        Some(execution_id.as_ref())
    );
    assert_eq!(
        first["evidence"]["schema_version"],
        serde_json::json!(EXECUTION_EVIDENCE_SCHEMA_VERSION)
    );
    assert!(first["evidence"]["gaps"]
        .as_array()
        .is_some_and(|gaps| gaps.iter().all(|gap| {
            gap["component"] != "execution_identity" || gap["code"] != "legacy_missing"
        })));
}

#[test]
fn run_record_constructor_replaces_a_callers_execution_identity() {
    let owner = crate::mint_execution_scope();
    let _scope = crate::observability::execution_scope::enter_execution_scope(owner.clone());
    let run = constructed_run_json(serde_json::json!({
        "evidence": {
            "schema_version": EXECUTION_EVIDENCE_SCHEMA_VERSION,
            "execution_id": "hxe-019c13e0-8080-7000-8000-000000000099"
        }
    }));

    assert_eq!(
        run["evidence"]["execution_id"].as_str(),
        Some(owner.as_ref())
    );
}

#[test]
fn run_record_constructor_rejects_a_missing_execution_owner() {
    let error = run_record_impl(
        &[crate::stdlib::json_to_vm_value(&serde_json::json!({
            "evidence": {
                "schema_version": EXECUTION_EVIDENCE_SCHEMA_VERSION,
                "execution_id": "hxe-019c13e0-8080-7000-8000-000000000099"
            }
        }))],
        &mut String::new(),
    )
    .expect_err("scope-less construction must fail closed");

    assert!(error
        .to_string()
        .contains("run_record: active execution scope unavailable"));
}

#[test]
fn run_record_constructor_rejects_evidence_owned_by_another_execution() {
    let _scope = crate::enter_execution_scope(crate::mint_execution_scope());
    let error = run_record_impl(
        &[crate::stdlib::json_to_vm_value(&serde_json::json!({
            "evidence": {
                "schema_version": EXECUTION_EVIDENCE_SCHEMA_VERSION,
                "flight_recording": {
                    "schema_version": crate::flight_recorder::FLIGHT_RECORDING_SCHEMA_VERSION,
                    "execution_id": "hxe-019c13e0-8080-7000-8000-000000000099",
                    "format": crate::flight_recorder::FLIGHT_RECORDING_FORMAT,
                    "path": null,
                    "content_hash": format!("blake3:{}", "a".repeat(64)),
                    "byte_length": 0,
                    "retained_events": 0,
                    "dropped_events": 0,
                    "value_policy": "omitted"
                }
            }
        }))],
        &mut String::new(),
    )
    .expect_err("cross-execution artifact must fail closed");

    assert!(error
        .to_string()
        .contains("flight recording identity does not match"));
}

#[test]
fn generic_legacy_normalization_does_not_claim_the_readers_execution() {
    let _scope =
        crate::observability::execution_scope::enter_execution_scope(crate::mint_execution_scope());
    let legacy = normalize_run_record(&crate::stdlib::json_to_vm_value(
        &serde_json::json!({"id": "run-legacy"}),
    ))
    .expect("normalize legacy record");

    assert_eq!(legacy.evidence.execution_id, None);
    assert!(legacy
        .evidence
        .gaps
        .iter()
        .any(|gap| gap.component == "execution_identity" && gap.code == "legacy_missing"));
}
