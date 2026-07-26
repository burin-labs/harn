//! Tests for `AgentEvent::from_host_payload` — typed host-emit deserialization.
//!
//! These pin the accept/reject boundary and the per-field defaults that the
//! retired hand-written `build_agent_event` match applied, so the serde path
//! stays behavior-identical.

use super::*;
use serde_json::json;

#[test]
fn from_host_deserializes_generic_variant() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "iteration_start",
        &json!({ "iteration": 3, "provider": "openai", "model": "gpt" }),
    )
    .expect("iteration_start");
    match event {
        AgentEvent::IterationStart {
            session_id,
            iteration,
            provider,
            model,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(iteration, 3);
            assert_eq!(provider, "openai");
            assert_eq!(model, "gpt");
        }
        other => panic!("expected IterationStart, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_defaults_status_and_audit() {
    // No `status` in the payload -> Pending; audit comes from the (absent)
    // ambient mutation session -> None, never from the payload.
    let event = AgentEvent::from_host_payload(
        "s1",
        "tool_call",
        &json!({ "tool_call_id": "t1", "tool_name": "read_file", "audit": {"bogus": true} }),
    )
    .expect("tool_call");
    match event {
        AgentEvent::ToolCall {
            status,
            raw_input,
            audit,
            ..
        } => {
            assert_eq!(status, ToolCallStatus::Pending);
            assert_eq!(raw_input, serde_json::Value::Null);
            assert!(audit.is_none());
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_defaults_status_and_maps_executor_alias() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({
            "tool_call_id": "t1",
            "tool_name": "read_file",
            "mutation_status": "unknown",
            "executor": "harn",
        }),
    )
    .expect("tool_call_update");
    match event {
        AgentEvent::ToolCallUpdate {
            status, executor, ..
        } => {
            assert_eq!(status, ToolCallStatus::InProgress);
            assert_eq!(executor, Some(ToolExecutor::HarnBuiltin));
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_preserves_structured_executor() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({
            "tool_call_id": "t1",
            "tool_name": "linear_create",
            "status": "completed",
            "mutation_status": "unknown",
            "executor": { "kind": "mcp_server", "server_name": "linear" },
        }),
    )
    .expect("tool_call_update");
    match event {
        AgentEvent::ToolCallUpdate { executor, .. } => {
            assert_eq!(
                executor,
                Some(ToolExecutor::McpServer {
                    server_name: "linear".to_string()
                })
            );
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_preserves_structured_mutation_status() {
    for (wire, expected) in [
        ("applied", ToolMutationStatus::Applied),
        ("not_applied", ToolMutationStatus::NotApplied),
        ("unknown", ToolMutationStatus::Unknown),
    ] {
        let event = AgentEvent::from_host_payload(
            "s1",
            "tool_call_update",
            &json!({
                "tool_call_id": "t1",
                "tool_name": "edit",
                "status": "completed",
                "mutation_status": wire,
            }),
        )
        .expect("typed mutation status");
        assert!(matches!(
            event,
            AgentEvent::ToolCallUpdate {
                mutation_status: status,
                ..
            } if status == expected
        ));
    }
}

#[test]
fn from_host_tool_call_update_rejects_unknown_mutation_status() {
    let error = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({
            "tool_call_id": "t1",
            "tool_name": "edit",
            "status": "completed",
            "mutation_status": "maybe",
        }),
    )
    .expect_err("unknown mutation status rejected");
    assert!(format!("{error:?}").contains("unknown variant `maybe`"));
}

#[test]
fn from_host_tool_call_update_rejects_unknown_executor() {
    let error = AgentEvent::from_host_payload(
        "s1",
        "tool_call_update",
        &json!({ "tool_call_id": "t1", "tool_name": "x", "executor": "nope" }),
    )
    .expect_err("unknown executor rejected");
    assert!(format!("{error:?}").contains("invalid tool executor"));
}

#[test]
fn from_host_progress_reported_defaults_replace_true() {
    let event = AgentEvent::from_host_payload("s1", "progress_reported", &json!({}))
        .expect("progress_reported");
    match event {
        AgentEvent::ProgressReported {
            replace,
            entries,
            metadata,
            message,
            ..
        } => {
            assert!(replace, "replace defaults to true");
            assert_eq!(entries, json!([]));
            assert_eq!(metadata, json!({}));
            assert!(message.is_none());
        }
        other => panic!("expected ProgressReported, got {other:?}"),
    }
}

#[test]
fn from_host_verdict_deserializes_complete_payload() {
    // The loop always emits a normalized (complete) verdict payload; the
    // typed path deserializes it 1:1.
    let event = AgentEvent::from_host_payload(
        "s1",
        "scope_classifier_verdict",
        &json!({
            "iteration": 1,
            "label": "escalate",
            "original_label": "trivial",
            "confidence": 0.4,
            "confidence_threshold": 0.65,
            "evidence": "e",
            "skip_main_turn": false,
        }),
    )
    .expect("scope_classifier_verdict");
    match event {
        AgentEvent::ScopeClassifierVerdict {
            label,
            confidence,
            confidence_threshold,
            skip_main_turn,
            ..
        } => {
            assert_eq!(label, "escalate");
            assert_eq!(confidence, 0.4);
            assert_eq!(confidence_threshold, 0.65);
            assert!(!skip_main_turn);
        }
        other => panic!("expected ScopeClassifierVerdict, got {other:?}"),
    }
}

#[test]
fn from_host_input_guardrail_deserializes_complete_payload() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "input_guardrail_verdict",
        &json!({
            "iteration": 1,
            "tripwire": true,
            "reason": "policy denied",
            "label": "prompt_injection",
            "confidence": 0.97,
            "confidence_threshold": 0.8,
            "classifier_kind": "custom",
        }),
    )
    .expect("input_guardrail_verdict");
    match event {
        AgentEvent::InputGuardrailVerdict {
            tripwire,
            reason,
            label,
            confidence,
            classifier_kind,
            ..
        } => {
            assert!(tripwire);
            assert_eq!(reason, "policy denied");
            assert_eq!(label, "prompt_injection");
            assert_eq!(confidence, 0.97);
            assert_eq!(classifier_kind.as_deref(), Some("custom"));
        }
        other => panic!("expected InputGuardrailVerdict, got {other:?}"),
    }
}

#[test]
fn from_host_nudge_maps_to_feedback_injected() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "llm_auto_continue",
        &json!({
            "previous_max_tokens": 100,
            "raised_max_tokens": 400,
            "attempt": 1,
            "max_continuations": 3,
        }),
    )
    .expect("llm_auto_continue");
    match event {
        AgentEvent::FeedbackInjected { kind, content, .. } => {
            assert_eq!(kind, "llm_auto_continue");
            assert_eq!(content, "100->400 (attempt 1/3)");
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn from_host_no_progress_nudge_preserves_injected_text_and_streak() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "no_progress_streak_nudge",
        &json!({
            "iteration": 4,
            "content": "No progress last turn. Emit exactly one well-formed <tool_call> now.",
            "streak": 2,
            "turns_since_progress": 2,
            "has_tools": true,
            "made_tool_calls": false,
        }),
    )
    .expect("no_progress_streak_nudge");
    match event {
        AgentEvent::FeedbackInjected {
            kind,
            content,
            streak,
            ..
        } => {
            assert_eq!(kind, "no_progress_streak_nudge");
            assert_eq!(
                content,
                "No progress last turn. Emit exactly one well-formed <tool_call> now."
            );
            assert_eq!(streak, Some(2));
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn from_host_no_progress_nudge_without_text_uses_explanatory_fallback() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "no_progress_streak_nudge",
        &json!({
            "iteration": 4,
            "streak": 2,
            "turns_since_progress": 2,
            "has_tools": true,
            "made_tool_calls": false,
        }),
    )
    .expect("no_progress_streak_nudge");
    match event {
        AgentEvent::FeedbackInjected {
            kind,
            content,
            streak,
            ..
        } => {
            assert_eq!(kind, "no_progress_streak_nudge");
            assert_ne!(content, "2");
            assert!(content.contains("No progress was detected"));
            assert_eq!(streak, Some(2));
        }
        other => panic!("expected FeedbackInjected, got {other:?}"),
    }
}

#[test]
fn from_host_judge_decision_preserves_source() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "judge_decision",
        &json!({
            "iteration": 3,
            "verdict": "continue",
            "reasoning": "need verifier output",
            "next_step": "run tests",
            "judge_duration_ms": 0,
            "source": "deterministic",
            "trigger": "verify_completion",
            "reason": "repeated_verification_failures",
            "escalation_recommended": true,
            "escalation_target": "frontier",
            "specific_gaps": [],
            "accepted_evidence": [],
        }),
    )
    .expect("judge_decision");
    match event {
        AgentEvent::JudgeDecision {
            source,
            trigger,
            escalation_recommended,
            escalation_target,
            ..
        } => {
            assert_eq!(source.as_deref(), Some("deterministic"));
            assert_eq!(trigger.as_deref(), Some("verify_completion"));
            assert_eq!(escalation_recommended, Some(true));
            assert_eq!(escalation_target.as_deref(), Some("frontier"));
        }
        other => panic!("expected JudgeDecision, got {other:?}"),
    }
}

#[test]
fn from_host_stance_events_map_to_stance_transition() {
    let event = AgentEvent::from_host_payload(
        "s1",
        "stance_write_access_granted",
        &json!({
            "escape_tool": "grant_write",
            "allowed_tools": ["read_file", "write_file"],
            "consent": "operator",
        }),
    )
    .expect("stance_write_access_granted");
    match event {
        AgentEvent::StanceTransition {
            phase,
            escape_tool,
            allowed_tools,
            consent,
            ..
        } => {
            assert_eq!(phase, "write_access_granted");
            assert_eq!(escape_tool, "grant_write");
            assert_eq!(allowed_tools, vec!["read_file", "write_file"]);
            assert_eq!(consent, "operator");
        }
        other => panic!("expected StanceTransition, got {other:?}"),
    }
}

#[test]
fn from_host_deserializes_budget_events() {
    // Budget termination is a normal host-emitted loop outcome, so both
    // variants must stay inside the explicit host allowlist.
    match AgentEvent::from_host_payload(
        "s1",
        "budget_exhausted",
        &json!({ "kind": "budget", "max_iterations": 12, "iteration": 12, "cost_usd": 0.0, "wall_clock_ms": 0 }),
    )
    .expect("budget_exhausted")
    {
        AgentEvent::BudgetExhausted {
            max_iterations,
            kind,
            ..
        } => {
            assert_eq!(max_iterations, 12);
            assert_eq!(kind.as_deref(), Some("budget"));
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
    match AgentEvent::from_host_payload(
        "s1",
        "budget_circuit_breaker",
        &json!({ "kind": "consecutive_failures", "consecutive_count": 3, "paused_for_ms": 500 }),
    )
    .expect("budget_circuit_breaker")
    {
        AgentEvent::BudgetCircuitBreaker {
            consecutive_count,
            paused_for_ms,
            ..
        } => {
            assert_eq!(consecutive_count, 3);
            assert_eq!(paused_for_ms, 500);
        }
        other => panic!("expected BudgetCircuitBreaker, got {other:?}"),
    }
}

#[test]
fn from_host_rejects_non_host_and_unknown_event_types() {
    // A real `AgentEvent` variant that is never emitted through the host
    // path stays rejected (parity with the retired match's allowlist).
    AgentEvent::from_host_payload("s1", "worker_update", &json!({}))
        .expect_err("worker_update is not a host-emittable event type");
    AgentEvent::from_host_payload("s1", "totally_made_up", &json!({}))
        .expect_err("unknown event type rejected");
}
