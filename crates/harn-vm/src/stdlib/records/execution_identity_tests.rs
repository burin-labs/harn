use std::sync::Arc;

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
    let execution_id: Arc<str> = Arc::from("hxe-constructor-test");
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
fn run_record_constructor_preserves_an_explicit_execution_identity() {
    let _scope = crate::observability::execution_scope::enter_execution_scope(Arc::from(
        "hxe-current-execution",
    ));
    let run = constructed_run_json(serde_json::json!({
        "evidence": {
            "schema_version": EXECUTION_EVIDENCE_SCHEMA_VERSION,
            "execution_id": "hxe-explicit"
        }
    }));

    assert_eq!(
        run["evidence"]["execution_id"].as_str(),
        Some("hxe-explicit")
    );
}

#[test]
fn generic_legacy_normalization_does_not_claim_the_readers_execution() {
    let _scope = crate::observability::execution_scope::enter_execution_scope(Arc::from(
        "hxe-reader-must-not-own-legacy-record",
    ));
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
