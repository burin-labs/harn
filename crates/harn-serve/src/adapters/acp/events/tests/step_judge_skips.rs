//! Split out of the parent test file, which sits on the source-length
//! ratchet's legacy list. These two cases are one contract: how a step-judge
//! decision that did NOT call a judge model reaches the wire, whether it was
//! short-circuited or the model was unavailable. The child reaches the
//! parent's fixtures through its own `use super::*`.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn step_judge_decision_agent_event_marks_skipped_reason() {
    let actual = collect_notifications(vec![AgentEvent::StepJudgeDecision {
        session_id: "session-1".to_string(),
        iteration: 1,
        verdict: "pass".to_string(),
        reasoning: String::new(),
        critique: String::new(),
        confidence: 1.0,
        judge_duration_ms: 0,
        vetoed: false,
        skipped: true,
        reason: Some("low_iteration_budget".to_string()),
        judge_error: false,
        on_veto: "replace".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        provider: String::new(),
        model: String::new(),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "step_judge_decision");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["skipped"], true);
    assert_eq!(params["reason"], "low_iteration_budget");
    assert_eq!(params["vetoed"], false);
    // A genuine budget skip is NOT a swallowed judge error.
    assert_eq!(params["judgeError"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn step_judge_decision_agent_event_surfaces_judge_unavailable() {
    // When the step-judge model errors and fail-open lets the turn through,
    // the decision must carry the distinct `judgeError` marker so a
    // fail-open swallow is observable, not indistinguishable from a real pass.
    let actual = collect_notifications(vec![AgentEvent::StepJudgeDecision {
        session_id: "session-1".to_string(),
        iteration: 1,
        verdict: "pass".to_string(),
        reasoning: "judge backend 503".to_string(),
        critique: String::new(),
        confidence: 0.0,
        judge_duration_ms: 0,
        vetoed: false,
        skipped: true,
        reason: Some("judge_unavailable".to_string()),
        judge_error: true,
        on_veto: "replace".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        provider: String::new(),
        model: String::new(),
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "step_judge_decision");
    assert_eq!(params["verdict"], "pass");
    assert_eq!(params["reason"], "judge_unavailable");
    assert_eq!(params["judgeError"], true);
}
