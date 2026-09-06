use super::*;

#[tokio::test(flavor = "current_thread")]
async fn feedback_injected_streak_reaches_acp_payload() {
    let actual = collect_notifications(vec![AgentEvent::FeedbackInjected {
        session_id: "session-1".to_string(),
        kind: "no_progress_streak_nudge".to_string(),
        content: "No progress last turn. Emit exactly one tool call now.".to_string(),
        streak: Some(2),
        iteration: None,
        tool_name: None,
        turn_claimed_for_repair: None,
        delivered: None,
    }])
    .await;

    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0]["method"], HARN_AGENT_EVENT_METHOD);
    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "feedback_injected");
    assert_eq!(params["feedbackKind"], "no_progress_streak_nudge");
    assert_eq!(
        params["content"],
        "No progress last turn. Emit exactly one tool call now."
    );
    assert_eq!(params["streak"], 2);
}

#[tokio::test(flavor = "current_thread")]
async fn repair_claim_reaches_acp_payload_as_typed_fields() {
    let actual = collect_notifications(vec![AgentEvent::FeedbackInjected {
        session_id: "session-1".to_string(),
        kind: "missing_tool_call_nudge".to_string(),
        content: "corrective wording is not the contract".to_string(),
        streak: None,
        iteration: Some(7),
        tool_name: Some("edit".to_string()),
        turn_claimed_for_repair: Some(true),
        delivered: None,
    }])
    .await;

    assert_eq!(actual.len(), 1);
    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "feedback_injected");
    assert_eq!(params["iteration"], 7);
    assert_eq!(params["toolName"], "edit");
    assert_eq!(params["turnClaimedForRepair"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn a_receipt_that_delivered_nothing_says_so_on_the_wire() {
    // `false` is the reading that matters. A projection that only forwards
    // `true` turns "the mechanism ran and delivered nothing" back into
    // "the mechanism did not answer", which is the loss this field exists
    // to close.
    let actual = collect_notifications(vec![AgentEvent::FeedbackInjected {
        session_id: "session-1".to_string(),
        kind: "no_progress_streak_nudge".to_string(),
        content: "receipt only".to_string(),
        streak: Some(1),
        iteration: Some(3),
        tool_name: None,
        turn_claimed_for_repair: None,
        delivered: Some(false),
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(
        params["delivered"],
        serde_json::json!(false),
        "a false delivery must survive the projection"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_receipt_that_never_answered_the_question_omits_the_field() {
    // The negative control for the case above: absent stays absent, so a
    // reader can still tell "not delivered" from "not measured".
    let actual = collect_notifications(vec![AgentEvent::FeedbackInjected {
        session_id: "session-1".to_string(),
        kind: "user".to_string(),
        content: "continue".to_string(),
        streak: None,
        iteration: None,
        tool_name: None,
        turn_claimed_for_repair: None,
        delivered: None,
    }])
    .await;

    let params = &actual[0]["params"];
    assert!(
        params.get("delivered").is_none(),
        "an unanswered question must not read as a delivery: {params}"
    );
}
