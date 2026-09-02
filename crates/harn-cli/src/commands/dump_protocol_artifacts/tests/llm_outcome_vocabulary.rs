//! Conformance for the LLM outcome vocabularies on the
//! `harn.acp.prompt_error.v1` envelope.
//!
//! The class this guards: `category`/`kind`/`reason` used to leave Harn as
//! bare strings with no exported owner, so a host could not tell a value Harn
//! emits from one it invented. These tests bind the owner (`harn_vm`), the
//! envelope projection (`harn_serve`), and the generated Rust binding
//! together, and prove the unknown escape preserves a value the binding
//! predates instead of folding it into a known variant.

use harn_serve::adapters::acp::{AcpPromptErrorData, AcpPromptFailureFacts};
use harn_vm::llm::AgentTerminalClass;

use super::generated_rust_binding::{HarnLlmErrorCategory, HarnLlmErrorKind, HarnLlmErrorReason};
use super::*;

/// The generated binding must carry exactly the owner's vocabulary. If this
/// drifts, the artifact is stale and every consumer of it is guessing.
#[test]
fn generated_rust_enums_match_the_owning_vocabularies() {
    let known = |values: Vec<String>, generated: Vec<String>| {
        assert_eq!(values, generated);
    };
    known(
        llm_error_reason_values(),
        HarnLlmErrorReason::KNOWN
            .iter()
            .map(|reason| reason.as_str().to_string())
            .collect(),
    );
    known(
        llm_error_kind_values(),
        HarnLlmErrorKind::KNOWN
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect(),
    );
    known(
        llm_error_category_values(),
        HarnLlmErrorCategory::KNOWN
            .iter()
            .map(|category| category.as_str().to_string())
            .collect(),
    );
}

/// A `network_error` failure travels the production projection and decodes
/// into the typed reason on the far side.
#[test]
fn network_error_failure_round_trips_through_the_generated_rust_types() {
    // Shaped exactly as `harn_vm`'s provider-error path throws it: the
    // classifier's `reason`/`kind` strings plus the category it owns.
    let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!({
        "category": "transient_network",
        "kind": "transient",
        "reason": "network_error",
        "message": "error sending request: connection reset by peer",
    }));
    let envelope = serde_json::to_value(AcpPromptErrorData::with_facts(
        AgentTerminalClass::ProviderUnavailable,
        facts,
    ))
    .expect("envelope serializes");

    let reason: HarnLlmErrorReason =
        serde_json::from_value(envelope["reason"].clone()).expect("reason decodes");
    let kind: HarnLlmErrorKind =
        serde_json::from_value(envelope["kind"].clone()).expect("kind decodes");
    let category: HarnLlmErrorCategory =
        serde_json::from_value(envelope["category"].clone()).expect("category decodes");

    assert_eq!(reason, HarnLlmErrorReason::NetworkError);
    assert_eq!(kind, HarnLlmErrorKind::Transient);
    assert_eq!(category, HarnLlmErrorCategory::TransientNetwork);
    assert!(reason.is_known() && kind.is_known() && category.is_known());

    // Re-serializing returns the same wire strings the producer wrote.
    assert_eq!(
        serde_json::to_value(&reason).expect("reason serializes"),
        envelope["reason"]
    );
    assert_eq!(
        serde_json::to_value(&kind).expect("kind serializes"),
        envelope["kind"]
    );
}

/// Negative control for the unknown escape.
///
/// `provider_connection_failed` is the value a host invented because no typed
/// owner existed. It is not in Harn's vocabulary and never will be. A binding
/// generated before some future reason exists must land such a value in
/// `Unrecognized` and hand it back unchanged, never fold it into a neighbour
/// and never silently drop it.
#[test]
fn a_value_outside_the_vocabulary_lands_in_the_unknown_escape() {
    let reason: HarnLlmErrorReason =
        serde_json::from_value(serde_json::json!("provider_connection_failed"))
            .expect("an unrecognized reason still decodes");
    assert_eq!(
        reason,
        HarnLlmErrorReason::Unrecognized("provider_connection_failed".to_string())
    );
    assert!(!reason.is_known());
    assert_eq!(reason.as_str(), "provider_connection_failed");
    assert_eq!(
        serde_json::to_value(&reason).expect("serializes"),
        serde_json::json!("provider_connection_failed")
    );

    // `unknown` is a real Harn reason and must NOT be confused with the
    // escape: "Harn classified this as unknown" is a different fact from
    // "this binding does not recognize the string".
    let classified_unknown: HarnLlmErrorReason =
        serde_json::from_value(serde_json::json!("unknown")).expect("decodes");
    assert_eq!(classified_unknown, HarnLlmErrorReason::Unknown);
    assert!(classified_unknown.is_known());

    let kind: HarnLlmErrorKind =
        serde_json::from_value(serde_json::json!("provider_degraded")).expect("decodes");
    assert_eq!(
        kind,
        HarnLlmErrorKind::Unrecognized("provider_degraded".to_string())
    );
    assert!(!kind.is_known());
}
