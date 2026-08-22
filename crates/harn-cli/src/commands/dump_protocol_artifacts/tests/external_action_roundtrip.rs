use super::*;

#[test]
fn generated_rust_external_action_activity_round_trips() {
    use generated_rust_binding::{
        HarnActivityKind, HarnExternalActionActivityRecord, HarnExternalActionActivityStatus,
        HarnExternalActionDecider, HarnExternalActionDecisionOutcome,
        HarnExternalActionProtectedFieldClass,
    };

    let fixture = json!({
        "schema": "harn.external_action_activity.v1",
        "kind": "external_action",
        "id": "activity_abc123",
        "action_id": "action_abc123",
        "effect_fingerprint": "sha256:effect123",
        "intent_fingerprint": "sha256:abc123",
        "provider": "duffel",
        "capability": "flights",
        "operation": "create_order",
        "environment": "test",
        "summary": "Create one test flight order",
        "external_spend": {"currency": "USD", "amount_minor": 28381},
        "status": "confirmed",
        "updated_at_ms": 1_788_000_000_000_i64,
        "requester": {
            "actor": {"kind": "user", "id": "local-user"},
            "agent_id": "assistant",
            "model_provider": "openai",
            "model_id": "test-model",
            "session_id": "session-1",
            "run_id": "run-1"
        },
        "policy_evaluations": [{
            "layer": "managed_policy",
            "outcome": "allowed",
            "reason_code": "test_mode_allowed",
            "policy_id": "managed-default"
        }],
        "decision": {
            "outcome": "approved",
            "decider": "person",
            "decided_at_ms": 1_788_000_000_000_i64,
            "reason_code": "approved_exact_action",
            "actor": {"kind": "user", "id": "local-user"}
        },
        "authorization": {
            "method": "manual",
            "authentication_assurance": "session",
            "issued_at_ms": 1_788_000_000_000_i64,
            "expires_at_ms": 1_788_000_300_000_i64
        },
        "disclosure": {
            "recipient": "Duffel test mode",
            "purpose": "Create one test flight order",
            "field_classes": ["legal_identity", "birth_date"],
            "source": "fictional_test_fixture",
            "authentication_assurance": "session"
        },
        "dispatch": {"attempted": true, "adapter_id": "duffel-test-v1"},
        "reconciliation": {"attempted": false, "status": "not_needed"},
        "retry": {
            "schema": "harn.external_action_retry_link.v1",
            "previous_action_id": "action_denied",
            "previous_receipt_id": "receipt_denied"
        },
        "receipt": {
            "schema": "harn.external_action_receipt.v1",
            "id": "receipt-1",
            "action_id": "action_abc123",
            "effect_fingerprint": "sha256:effect123",
            "intent_fingerprint": "sha256:abc123",
            "idempotency_key": "idempotency-1",
            "provider": "duffel",
            "capability": "flights",
            "operation": "create_order",
            "environment": "test",
            "adapter_id": "duffel-test-v1",
            "outcome": "confirmed",
            "status": "confirmed",
            "next_action": "none",
            "dispatch_attempted": true,
            "recorded_at_ms": 1_788_000_000_000_i64,
            "provider_action_id": "ord_test_1",
            "evidence_refs": ["provider:order:ord_test_1"],
            "retry": {
                "schema": "harn.external_action_retry_link.v1",
                "previous_action_id": "action_denied",
                "previous_receipt_id": "receipt_denied"
            },
            "disclosure": {
                "recipient": "Duffel test mode",
                "purpose": "Create one test flight order",
                "field_classes": ["legal_identity", "birth_date"],
                "source": "fictional_test_fixture",
                "authentication_assurance": "session"
            }
        }
    });

    let decoded: HarnExternalActionActivityRecord =
        serde_json::from_value(fixture.clone()).expect("generated activity DTO decodes");
    assert_eq!(decoded.kind, HarnActivityKind::ExternalAction);
    assert_eq!(decoded.status, HarnExternalActionActivityStatus::Confirmed);
    assert_eq!(
        decoded.retry.as_ref().expect("retry").previous_receipt_id,
        "receipt_denied"
    );
    assert_eq!(
        decoded
            .receipt
            .as_ref()
            .expect("receipt")
            .effect_fingerprint
            .as_deref(),
        Some("sha256:effect123")
    );
    assert_eq!(
        decoded.decision.as_ref().expect("decision").outcome,
        HarnExternalActionDecisionOutcome::Approved
    );
    assert_eq!(
        decoded.decision.as_ref().expect("decision").decider,
        HarnExternalActionDecider::Person
    );
    assert_eq!(
        decoded
            .disclosure
            .as_ref()
            .expect("disclosure")
            .field_classes,
        [
            HarnExternalActionProtectedFieldClass::LegalIdentity,
            HarnExternalActionProtectedFieldClass::BirthDate,
        ]
    );
    assert_eq!(
        serde_json::to_value(decoded).expect("generated activity DTO encodes"),
        fixture
    );
}
