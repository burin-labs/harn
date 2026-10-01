use super::parse_review_verdict;

fn parse(
    approved: bool,
    disposition: &str,
    outcome: serde_json::Value,
) -> super::ApprovalReviewOutcome {
    parse_review_verdict(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "approved": approved,
        "reviewer_answered": true,
        "rationale": "fixture",
        "evaluation_review": {
            "disposition": disposition,
            "rule": "fixture_rule",
            "outcome": outcome,
        },
    })))
}

#[test]
fn non_answers_cannot_become_grants_or_considered_rejections() {
    let outcomes = [
        serde_json::json!({"kind": "low_confidence", "candidates": {}, "threshold": 0.99, "question_ids": [], "receipt": "low"}),
        serde_json::json!({"kind": "refused", "reason": "provider_refusal", "diagnostic": "fixture", "receipt": "refused"}),
        serde_json::json!({"kind": "unavailable", "reason": "model_unconfigured", "receipt": "unavailable"}),
    ];
    for outcome in outcomes {
        let fallback = parse(false, "needs_review", outcome.clone());
        assert!(!fallback.approved);
        assert!(!fallback.reviewer_answered);
        assert_eq!(
            fallback.unavailable_reason.as_deref(),
            outcome["kind"].as_str()
        );
        assert!(
            fallback.evaluation_review.is_some(),
            "valid non-answers retain their metadata"
        );
        for disposition in ["approved", "denied"] {
            let invalid = parse(disposition == "approved", disposition, outcome.clone());
            assert!(!invalid.approved);
            assert_eq!(
                invalid.unavailable_reason.as_deref(),
                Some("non_answer_cannot_settle_review")
            );
        }
    }
}

#[test]
fn malformed_and_inconsistent_decision_evidence_fail_closed() {
    let missing_receipt = parse(
        true,
        "approved",
        serde_json::json!({"kind": "answered", "value": {}}),
    );
    assert!(!missing_receipt.approved);
    assert_eq!(
        missing_receipt.unavailable_reason.as_deref(),
        Some("invalid_evaluation_outcome")
    );
    let inconsistent = parse(
        true,
        "needs_review",
        serde_json::json!({"kind": "answered", "value": {}, "receipt": "fixture"}),
    );
    assert!(!inconsistent.approved);
    assert_eq!(
        inconsistent.unavailable_reason.as_deref(),
        Some("inconsistent_review_disposition")
    );
}

#[test]
fn completed_decisions_preserve_grants_and_considered_rejections() {
    for approved in [true, false] {
        let review = parse(
            approved,
            if approved { "approved" } else { "denied" },
            serde_json::json!({"kind": "answered", "value": {}, "receipt": "fixture"}),
        );
        assert_eq!(review.approved, approved);
        assert!(review.reviewer_answered);
        assert!(review.unavailable_reason.is_none());
        assert!(review.evaluation_review.is_some());
    }
}
