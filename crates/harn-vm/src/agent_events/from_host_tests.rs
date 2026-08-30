//! Tests for `AgentEvent::from_host_payload` — typed host-emit deserialization.
//!
//! These pin the accept/reject boundary and the per-field defaults that the
//! retired hand-written `build_agent_event` match applied, so the serde path
//! stays behavior-identical.

use super::*;
use serde_json::json;

fn accepted_host_event(
    session_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> AgentEvent {
    AgentEvent::from_host_payload(session_id, event_type, payload)
        .unwrap_or_else(|error| {
            panic!("expected accepted host event `{event_type}`, got error {error:?}")
        })
        .unwrap_or_else(|| {
            panic!("expected accepted host event `{event_type}`, got unknown-type drop")
        })
}

fn capture_host_ingest_warnings(body: impl FnOnce()) -> Vec<crate::events::LogEvent> {
    use crate::events::{add_event_sink, clear_event_sinks, reset_event_sinks, CollectorSink};
    use std::rc::Rc;
    let sink = Rc::new(CollectorSink::new());
    clear_event_sinks();
    add_event_sink(sink.clone());
    body();
    let warnings = sink
        .logs
        .borrow()
        .iter()
        .filter(|event| event.category == "host_event_ingest")
        .cloned()
        .collect();
    reset_event_sinks();
    warnings
}

#[test]
fn from_host_deserializes_generic_variant() {
    let event = accepted_host_event(
        "s1",
        "iteration_start",
        &json!({ "iteration": 3, "provider": "openai", "model": "gpt" }),
    );
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
fn capability_gap_preserves_unresolved_route_diagnosis() {
    let event = accepted_host_event(
        "s1",
        "capability_gap",
        &json!({
            "level": "warning",
            "capability": "preferred_tool_format",
            "provider": "anthropic",
            "model": "unknown",
            "resolution": "unresolved",
            "reason": "missing_model",
            "fallback_tool_format": "json",
            "message": "route is unresolved",
        }),
    );

    match event {
        AgentEvent::CapabilityGap {
            resolution,
            reason,
            fallback_tool_format,
            ..
        } => {
            assert_eq!(resolution.as_deref(), Some("unresolved"));
            assert_eq!(reason.as_deref(), Some("missing_model"));
            assert_eq!(fallback_tool_format, "json");
        }
        other => panic!("expected CapabilityGap, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_defaults_status_and_audit() {
    // No `status` in the payload -> Pending; audit comes from the (absent)
    // ambient mutation session -> None, never from the payload.
    let event = accepted_host_event(
        "s1",
        "tool_call",
        &json!({ "tool_call_id": "t1", "tool_name": "read_file", "audit": {"bogus": true} }),
    );
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
    let event = accepted_host_event(
        "s1",
        "tool_call_update",
        &json!({
            "tool_call_id": "t1",
            "tool_name": "read_file",
            "mutation_status": "unknown",
            "data": {
                "command_status": "succeeded",
                "run_outcome": {"exit_code": 0}
            },
            "executor": "harn",
        }),
    );
    match event {
        AgentEvent::ToolCallUpdate {
            status,
            data,
            executor,
            ..
        } => {
            assert_eq!(status, ToolCallStatus::InProgress);
            assert_eq!(
                data,
                Some(json!({
                    "command_status": "succeeded",
                    "run_outcome": {"exit_code": 0}
                }))
            );
            assert_eq!(executor, Some(ToolExecutor::HarnBuiltin));
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[test]
fn from_host_tool_call_update_preserves_structured_executor() {
    let event = accepted_host_event(
        "s1",
        "tool_call_update",
        &json!({
            "tool_call_id": "t1",
            "tool_name": "linear_create",
            "status": "completed",
            "mutation_status": "unknown",
            "executor": { "kind": "mcp_server", "server_name": "linear" },
        }),
    );
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
        ("unchanged", ToolMutationStatus::Unchanged),
        ("not_applied", ToolMutationStatus::NotApplied),
        ("unknown", ToolMutationStatus::Unknown),
    ] {
        let event = accepted_host_event(
            "s1",
            "tool_call_update",
            &json!({
                "tool_call_id": "t1",
                "tool_name": "edit",
                "status": "completed",
                "mutation_status": wire,
            }),
        );
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
    let event = accepted_host_event("s1", "progress_reported", &json!({}));
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
fn progress_reported_uses_the_durable_internal_journal_policy() {
    assert_eq!(
        AgentEvent::host_transcript_role("progress_reported").map(|role| role.as_str()),
        Some("assistant"),
    );
}

#[test]
fn from_host_verdict_deserializes_complete_payload() {
    // The loop always emits a normalized (complete) verdict payload; the
    // typed path deserializes it 1:1.
    let event = accepted_host_event(
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
    );
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
    let event = accepted_host_event(
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
    );
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
    let event = accepted_host_event(
        "s1",
        "llm_auto_continue",
        &json!({
            "previous_max_tokens": 100,
            "raised_max_tokens": 400,
            "attempt": 1,
            "max_continuations": 3,
        }),
    );
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
    let event = accepted_host_event(
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
    );
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
    let event = accepted_host_event(
        "s1",
        "no_progress_streak_nudge",
        &json!({
            "iteration": 4,
            "streak": 2,
            "turns_since_progress": 2,
            "has_tools": true,
            "made_tool_calls": false,
        }),
    );
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
    let event = accepted_host_event(
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
            // Retired verdict fields a host built against an older contract
            // may still send. They must decode away, not fail the event.
            "specific_gaps": ["rerun the verifier"],
            "accepted_evidence": ["targeted verifier passed"],
        }),
    );
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
fn judge_started_enters_the_host_boundary_with_its_trigger() {
    let event = accepted_host_event(
        "s1",
        "judge_started",
        &json!({ "iteration": 4, "trigger": "verify_completion" }),
    );
    match event {
        AgentEvent::JudgeStarted {
            session_id,
            iteration,
            trigger,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(iteration, 4);
            assert_eq!(trigger.as_deref(), Some("verify_completion"));
        }
        other => panic!("expected JudgeStarted, got {other:?}"),
    }
}

#[test]
fn judge_started_without_a_trigger_still_opens_the_window() {
    // The window a host renders must not depend on an optional label; a judge
    // invoked without a named trigger still costs the user the same wait.
    let event = accepted_host_event("s1", "judge_started", &json!({ "iteration": 0 }));
    match event {
        AgentEvent::JudgeStarted { trigger, .. } => assert_eq!(trigger, None),
        other => panic!("expected JudgeStarted, got {other:?}"),
    }
}

#[test]
fn from_host_stance_events_map_to_stance_transition() {
    let event = accepted_host_event(
        "s1",
        "stance_write_access_granted",
        &json!({
            "escape_tool": "grant_write",
            "allowed_tools": ["read_file", "write_file"],
            "consent": "operator",
        }),
    );
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
    match accepted_host_event(
        "s1",
        "budget_exhausted",
        &json!({
            "kind": "total_cost",
            "max_iterations": 12,
            "iteration": 12,
            "cost_usd": 0.69,
            "wall_clock_ms": 0,
            "limit": "total_cost",
            "limit_value": 1.25,
            "projected_cost_usd": 0.75,
            "session_cost_usd": 0.69,
            "projected_input_tokens": 12000,
            "projected_output_tokens": 4000,
            "projection_basis": "observed",
            "headroom_usd": 0.56,
            "provider": "openai",
            "model": "gpt-5.6-sol",
        }),
    ) {
        AgentEvent::BudgetExhausted {
            max_iterations,
            kind,
            limit,
            limit_value,
            projected_cost_usd,
            session_cost_usd,
            projected_input_tokens,
            projected_output_tokens,
            projection_basis,
            headroom_usd,
            provider,
            model,
            ..
        } => {
            assert_eq!(max_iterations, 12);
            assert_eq!(kind.as_deref(), Some("total_cost"));
            assert_eq!(limit.as_deref(), Some("total_cost"));
            assert_eq!(
                limit_value.as_ref().and_then(serde_json::Number::as_f64),
                Some(1.25)
            );
            assert_eq!(projected_cost_usd, Some(0.75));
            assert_eq!(session_cost_usd, Some(0.69));
            assert_eq!(projected_input_tokens, Some(12000));
            assert_eq!(projected_output_tokens, Some(4000));
            assert_eq!(projection_basis.as_deref(), Some("observed"));
            assert_eq!(headroom_usd, Some(0.56));
            assert_eq!(provider.as_deref(), Some("openai"));
            assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
    match accepted_host_event(
        "s1",
        "budget_circuit_breaker",
        &json!({ "kind": "consecutive_failures", "consecutive_count": 3, "paused_for_ms": 500 }),
    ) {
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
fn documented_stdlib_events_are_typed_and_journaled() {
    let required_tools = accepted_host_event(
        "s1",
        "require_successful_tools_violation",
        &json!({
            "kind": "tool_gap",
            "source": "agent_loop.require_successful_tools",
            "actor": "implementer",
            "run_id": "s1",
            "redacted_summary": "missing edit",
            "recurrence_hints": ["missing_required_tools=1"],
            "metadata": {
                "missing_required_tools": ["edit"],
                "successful_tool_names": [],
                "iterations": 2,
            },
        }),
    );
    assert!(matches!(
        required_tools,
        AgentEvent::RequireSuccessfulToolsViolation {
            actor: Some(ref actor),
            ..
        } if actor == "implementer"
    ));

    let final_wrapup = accepted_host_event(
        "s1",
        "final_wrapup",
        &json!({
            "final_status": "max_iterations",
            "stop_reason": "iteration_limit",
            "iteration": 4,
            "host_directive": false,
            "terminal_kind": "max_iterations",
            "unconsumed_tool_call": {
                "parse_status": "partial",
                "parsed_call_count": 1,
                "tool_names": ["edit"],
                "diagnostics": ["second call was malformed"],
                "evidence_line": "the final summary contained an unconsumed tool call",
            },
        }),
    );
    assert!(matches!(
        final_wrapup,
        AgentEvent::FinalWrapup {
            iteration: 4,
            unconsumed_tool_call: Some(ref evidence),
            ..
        } if evidence.parsed_call_count == 1
    ));

    let legacy_final_wrapup = accepted_host_event(
        "s1",
        "final_wrapup",
        &json!({
            "final_status": "max_iterations",
            "stop_reason": "iteration_limit",
            "iteration": 4,
            "host_directive": false,
            "terminal_kind": "max_iterations",
        }),
    );
    assert!(matches!(
        legacy_final_wrapup,
        AgentEvent::FinalWrapup {
            unconsumed_tool_call: None,
            ..
        }
    ));

    let thinking = accepted_host_event(
        "s1",
        "pack_thinking_stripped",
        &json!({
            "model": "claude-opus-adaptive",
            "requested": "high",
            "reason": "claude_opus_adaptive",
        }),
    );
    assert!(matches!(
        thinking,
        AgentEvent::PackThinkingStripped { ref requested, .. } if requested == "high"
    ));

    let tie = accepted_host_event(
        "s1",
        "self_consistency_tie",
        &json!({
            "answer": "alpha",
            "total": 4,
            "distribution": [
                {"answer": "alpha", "count": 2},
                {"answer": "beta", "count": 2},
            ],
        }),
    );
    assert!(matches!(
        tie,
        AgentEvent::SelfConsistencyTie { total: 4, .. }
    ));

    let fallback = accepted_host_event(
        "s1",
        "code_librarian_query_nl_fallback",
        &json!({
            "attempted_cypher": null,
            "mcts_depth": 3,
            "mcts_expansions": 9,
            "result_count": 2,
            "text": "where is session recovery implemented?",
        }),
    );
    assert!(matches!(
        fallback,
        AgentEvent::CodeLibrarianQueryNlFallback {
            attempted_cypher: None,
            mcts_depth: 3,
            ..
        }
    ));

    for event_type in [
        "require_successful_tools_violation",
        "final_wrapup",
        "pack_thinking_stripped",
        "self_consistency_tie",
        "code_librarian_query_nl_fallback",
    ] {
        assert_eq!(
            AgentEvent::host_transcript_role(event_type).map(|role| role.as_str()),
            Some("assistant"),
            "{event_type} must use the same registry for host acceptance and journaling",
        );
    }
}

#[test]
fn from_host_drops_non_host_and_unknown_event_types_without_failing() {
    crate::agent_sessions::reset_session_store();
    // A real `AgentEvent` variant that is never emitted through the host
    // path is the same class as a never-seen name: drop, do not fail.
    assert!(
        AgentEvent::from_host_payload("s1", "worker_update", &json!({}))
            .expect("non-host type must not fail ingest")
            .is_none()
    );
    assert!(
        AgentEvent::from_host_payload("s1", "totally_made_up", &json!({}))
            .expect("unknown type must not fail ingest")
            .is_none()
    );
}

#[test]
fn from_host_accepts_model_job_events_and_journals_them_as_tool_activity() {
    let payload = json!({
        "schema": "harn.model_job_event.v1",
        "kind": "state_changed",
        "job_id": "job-1",
        "state": "running",
    });
    let event = accepted_host_event("s1", "model_job", &payload);
    assert!(matches!(
        event,
        AgentEvent::ModelJob {
            session_id,
            event,
        } if session_id == "s1" && event == payload
    ));
    assert_eq!(
        AgentEvent::host_transcript_role("model_job").map(|role| role.as_str()),
        Some("tool"),
    );
}

#[test]
fn from_host_accepts_typed_tool_batch_disposition_receipts() {
    let event = accepted_host_event(
        "s1",
        "tool_batch_disposition",
        &json!({
            "receipt": {
                "schema": "harn.agent_tool_batch_disposition.v1",
                "batch_id": "a".repeat(64),
                "source_batch_id": null,
                "call_index": 1,
                "tool_call_id": "call-2",
                "tool_name": "edit",
                "phase": "mutation",
                "selected_phase": "observation",
                "disposition": "deferred",
                "proposal_status": "new",
                "reason": "effect_phase_boundary",
                "planned_at_ms": 12.0,
                "started_at_ms": null,
                "finished_at_ms": null,
                "duration_ms": null,
                "blocking_tool_call_id": null,
                "blocking_tool_name": null,
                "blocking_mutation_status": null
            }
        }),
    );

    match event {
        AgentEvent::ToolBatchDisposition {
            session_id,
            receipt,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(receipt.tool_name, "edit");
            assert_eq!(
                receipt.disposition,
                super::agent::ToolBatchDisposition::Deferred
            );
        }
        other => panic!("expected ToolBatchDisposition, got {other:?}"),
    }
}

// --- Loud-boundary funnel (harn#5142) ------------------------------------

/// A `.harn` boundary reports through the same typed event as the Rust funnel,
/// and `owner` is derived from `kind` here rather than trusted from the
/// payload, so one attribution rule covers both languages.
#[test]
fn from_host_accepts_a_harn_side_boundary_failure_and_derives_the_owner() {
    let event = accepted_host_event(
        "s1",
        "boundary_failure",
        &json!({
            "boundary": "text_tool_parse",
            "kind": "capped",
            "owner": "agent",
            "detail": "tool-shaped bytes were not parsed",
            "dropped_count": 1,
        }),
    );
    match event {
        AgentEvent::BoundaryFailure {
            session_id,
            boundary,
            kind,
            owner,
            detail,
            dropped_count,
            unreported,
            ..
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(boundary, crate::boundary::BoundaryId::TextToolParse);
            assert_eq!(kind, crate::boundary::BoundaryFailureKind::Capped);
            assert_eq!(
                owner, "policy",
                "a payload-supplied owner must be overruled, not trusted",
            );
            assert!(detail.contains("tool-shaped bytes"));
            assert_eq!(dropped_count, 1);
            assert!(!unreported);
        }
        other => panic!("expected BoundaryFailure, got {other:?}"),
    }
}

/// The `host_event_ingest` boundary. Unknown names warn and drop. A
/// malformed payload of a registered type still fails the emit and
/// reports through the funnel, because every stdlib site wraps
/// `agent_emit_event` in `try { }` and would otherwise swallow the error.
#[test]
fn a_malformed_known_host_event_reports_through_the_funnel() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    AgentEvent::from_host_payload(
        "s1",
        "iteration_start",
        &json!({ "iteration": "not-a-number" }),
    )
    .expect_err("malformed known type must stay fatal");
    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got {events:?}");
    match &events[0] {
        AgentEvent::BoundaryFailure {
            session_id,
            boundary,
            kind,
            owner,
            excerpt,
            ..
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(*boundary, crate::boundary::BoundaryId::HostEventIngest);
            assert_eq!(*kind, crate::boundary::BoundaryFailureKind::Unrecognized);
            assert_eq!(owner, "harness");
            assert!(excerpt.is_some(), "the rejected payload must ride along");
        }
        other => panic!("expected BoundaryFailure, got {other:?}"),
    }
}

#[test]
fn an_unknown_host_event_type_warns_once_per_session_and_is_not_a_boundary_failure() {
    crate::agent_sessions::reset_session_store();
    let captured = crate::boundary::tests::CapturedEvents::install();
    let warnings = capture_host_ingest_warnings(|| {
        for _ in 0..2 {
            let dropped = AgentEvent::from_host_payload("s1", "totally_made_up", &json!({}))
                .expect("unknown type must not fail ingest");
            assert!(
                dropped.is_none(),
                "unknown type must not become an AgentEvent"
            );
        }
        let other_session = AgentEvent::from_host_payload("s2", "totally_made_up", &json!({}))
            .expect("unknown type on a later session must not fail ingest");
        assert!(other_session.is_none());
        let other_type = AgentEvent::from_host_payload("s1", "also_made_up", &json!({}))
            .expect("a second unknown type must not fail ingest");
        assert!(other_type.is_none());
    });
    assert!(
        captured.boundary_failures().is_empty(),
        "unknown types must not surface as boundary_failure: {:?}",
        captured.boundary_failures()
    );
    assert_eq!(
        warnings.len(),
        3,
        "one warning per unknown type per session, got {warnings:?}"
    );
    assert_eq!(warnings[0].level, crate::events::EventLevel::Warn);
    assert_eq!(
        warnings[0].metadata.get("event_type"),
        Some(&json!("totally_made_up"))
    );
    assert_eq!(warnings[1].metadata.get("session_id"), Some(&json!("s2")));
    assert_eq!(
        warnings[2].metadata.get("event_type"),
        Some(&json!("also_made_up"))
    );
}

#[test]
fn an_unknown_host_event_warns_once_when_a_session_moves_threads() {
    fn call_on_thread(
        runtime: std::sync::Arc<crate::agent_sessions::AgentSessionRuntime>,
    ) -> Vec<crate::events::LogEvent> {
        std::thread::spawn(move || {
            let previous = crate::agent_sessions::swap_active_session_runtime(runtime);
            let warnings = capture_host_ingest_warnings(|| {
                let dropped =
                    AgentEvent::from_host_payload("migrating", "future_event", &json!({}))
                        .expect("unknown type must not fail ingest");
                assert!(dropped.is_none());
            });
            crate::agent_sessions::swap_active_session_runtime(previous);
            warnings
        })
        .join()
        .expect("warning worker thread must finish")
    }

    let runtime = crate::agent_sessions::fresh_session_runtime();
    let mut warnings = call_on_thread(runtime.clone());
    warnings.extend(call_on_thread(runtime));

    assert_eq!(warnings.len(), 1, "session migration warned more than once");
    assert_eq!(
        warnings[0].metadata.get("session_id"),
        Some(&json!("migrating"))
    );
    assert_eq!(
        warnings[0].metadata.get("event_type"),
        Some(&json!("future_event"))
    );
}

#[test]
fn an_accepted_host_event_reports_no_boundary_failure() {
    let captured = crate::boundary::tests::CapturedEvents::install();
    accepted_host_event("s1", "iteration_start", &json!({ "iteration": 1 }));
    assert!(captured.boundary_failures().is_empty());
}

// ---------------------------------------------------------------------------
// Emitter/registry drift.
//
// The embedded stdlib and `HOST_EVENT_POLICIES` are two halves of one
// contract: a `.harn` module names an event, the registry decides whether it
// may enter. Nothing mechanical connected them, so `std/llm::with_logging`
// shipped for releases while every `llm_call_log` it emitted was rejected at
// this boundary — per-call latency and route attribution silently absent from
// run records, visible only as `boundary_failure` noise in the event log.
// ---------------------------------------------------------------------------

/// The third top-level argument of an already-opened argument list, when that
/// argument is a plain string literal. A computed or variable event name
/// returns `None`: no static check can resolve it, and guessing would be worse
/// than skipping.
fn third_argument_literal(rest: &str) -> Option<String> {
    let mut depth = 0i32;
    let mut arg_index = 0usize;
    let mut in_string = false;
    let mut current = String::new();
    let mut literal = None;
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        if in_string {
            match ch {
                '\\' => {
                    chars.next();
                }
                '"' => {
                    in_string = false;
                    if arg_index == 2 {
                        literal = Some(std::mem::take(&mut current));
                    }
                }
                _ if arg_index == 2 => current.push(ch),
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                current.clear();
            }
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => break,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                arg_index += 1;
                if arg_index > 2 {
                    break;
                }
            }
            _ => {}
        }
    }
    // An interpolated literal is a computed name wearing a literal's clothes.
    literal.filter(|name| !name.contains("${"))
}

/// Event-type literals the module emits through the host path. Both the
/// builtin and the `__emit_event` wrapper take the name as their third
/// argument, so one shape covers every emitter.
#[expect(
    clippy::string_slice,
    reason = "offsets come from find/rfind on `source` plus ASCII literal lengths"
)]
fn emitted_event_types(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for call in ["agent_emit_event(", "__emit_event("] {
        let mut search = 0usize;
        while let Some(offset) = source[search..].find(call) {
            let call_start = search + offset;
            let args_start = call_start + call.len();
            search = args_start;
            let line_start = source[..call_start]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            // `fn __emit_event(...)` declares the wrapper; it does not emit.
            if source[line_start..call_start]
                .trim_start()
                .starts_with("fn ")
            {
                continue;
            }
            if let Some(name) = third_argument_literal(&source[args_start..]) {
                found.push(name);
            }
        }
    }
    found
}

#[test]
fn every_event_type_the_embedded_stdlib_emits_is_registered() {
    let registered: std::collections::BTreeSet<&str> =
        crate::agent_events::from_host::registered_host_event_types().collect();
    let mut unregistered: Vec<String> = harn_stdlib::STDLIB_SOURCES
        .iter()
        .flat_map(|entry| {
            emitted_event_types(entry.source)
                .into_iter()
                .map(move |event_type| (entry.module, event_type))
        })
        .filter(|(_, event_type)| !registered.contains(event_type.as_str()))
        .map(|(module, event_type)| format!("{module}: {event_type}"))
        .collect();
    unregistered.sort();
    unregistered.dedup();
    assert!(
        unregistered.is_empty(),
        "these embedded-stdlib emits can never enter the host boundary. Either add a \
         HOST_EVENT_POLICIES row (and an AgentEvent variant) or stop emitting them: {unregistered:?}"
    );
}

#[test]
fn the_emitter_scan_actually_finds_emitters() {
    // A scanner that silently matches nothing would make the drift check above
    // vacuously green, so pin that it sees a known emitter in the real sources.
    let found: std::collections::BTreeSet<String> = harn_stdlib::STDLIB_SOURCES
        .iter()
        .flat_map(|entry| emitted_event_types(entry.source))
        .collect();
    assert!(
        found.contains("llm_call_log"),
        "expected the `with_logging` emitter; scanner found {found:?}"
    );
    assert!(
        found.contains("typed_checkpoint"),
        "expected the checkpoint emitter; scanner found {found:?}"
    );
}

#[test]
fn llm_call_log_enters_the_host_boundary_with_its_route_and_latency() {
    let event = accepted_host_event(
        "s1",
        "llm_call_log",
        &json!({
            "event": "llm_call_log",
            "level": "info",
            "latency_ms": 3067,
            "model": "gpt-5.6-luna",
            "provider": "openai",
            "status": "ok",
            "iteration": 1,
            "attempt": 1,
            "prompt": "kept for an include_prompt caller"
        }),
    );
    match event {
        AgentEvent::LlmCallLog {
            session_id,
            model,
            provider,
            status,
            latency_ms,
            iteration,
            attempt,
            payload,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(model, "gpt-5.6-luna");
            assert_eq!(provider, "openai");
            assert_eq!(status, "ok");
            assert_eq!(latency_ms, 3067);
            assert_eq!(iteration, 1);
            assert_eq!(attempt, 1);
            assert_eq!(
                payload["prompt"], "kept for an include_prompt caller",
                "the full record must survive so include_prompt loses nothing"
            );
        }
        other => panic!("expected LlmCallLog, got {other:?}"),
    }
}

#[test]
fn every_llm_handler_event_keeps_its_typed_head_and_its_whole_record() {
    // One case per arm the drift check turned up. Each asserts the scalar a
    // consumer joins on plus that nothing was dropped on the way through.
    let routing = accepted_host_event(
        "s1",
        "llm_routing_decision",
        &json!({"route_index": -1, "route_name": "default", "used_default": true}),
    );
    match routing {
        AgentEvent::LlmRoutingDecision {
            route_index,
            route_name,
            used_default,
            ..
        } => {
            assert_eq!(
                route_index, -1,
                "a default fall-through must survive as -1, not saturate to 0"
            );
            assert_eq!(route_name, "default");
            assert!(used_default);
        }
        other => panic!("expected LlmRoutingDecision, got {other:?}"),
    }

    let fallback = accepted_host_event(
        "s1",
        "llm_fallback_attempt",
        &json!({"fallback_index": 2, "fallback_total": 3, "ok": false, "status": "rate_limit"}),
    );
    match fallback {
        AgentEvent::LlmFallbackAttempt {
            fallback_index,
            fallback_total,
            ok,
            status,
            ..
        } => {
            assert_eq!((fallback_index, fallback_total), (2, 3));
            assert!(!ok);
            assert_eq!(status, "rate_limit");
        }
        other => panic!("expected LlmFallbackAttempt, got {other:?}"),
    }

    let shadow = accepted_host_event(
        "s1",
        "llm_shadow_diff",
        &json!({"primary_ok": true, "shadow_ok": false, "primary_status": "ok",
                "shadow_status": "timeout", "primary_len": 120, "shadow_len": 0}),
    );
    match shadow {
        AgentEvent::LlmShadowDiff {
            primary_ok,
            shadow_ok,
            shadow_status,
            primary_len,
            ..
        } => {
            assert!(primary_ok && !shadow_ok);
            assert_eq!(shadow_status, "timeout");
            assert_eq!(primary_len, 120);
        }
        other => panic!("expected LlmShadowDiff, got {other:?}"),
    }

    let hit = accepted_host_event(
        "s1",
        "semantic_cache_hit",
        &json!({"similarity": 0.94, "provider": "openai", "model": "gpt-5.6-luna",
                "metrics": {"model_calls_avoided": 1}}),
    );
    match hit {
        AgentEvent::SemanticCacheHit {
            similarity,
            provider,
            payload,
            ..
        } => {
            assert!((similarity - 0.94).abs() < f64::EPSILON);
            assert_eq!(provider, "openai");
            assert_eq!(
                payload["metrics"]["model_calls_avoided"], 1,
                "the cost-moat receipt must ride along like CacheHit's"
            );
        }
        other => panic!("expected SemanticCacheHit, got {other:?}"),
    }

    let miss = accepted_host_event(
        "s1",
        "semantic_cache_miss",
        &json!({"nearest_similarity": 0.61, "metrics": {"compute_ms": 42}}),
    );
    match miss {
        AgentEvent::SemanticCacheMiss {
            nearest_similarity,
            payload,
            ..
        } => {
            assert!(
                (nearest_similarity - 0.61).abs() < f64::EPSILON,
                "how close the best candidate came is the whole diagnostic value"
            );
            assert_eq!(payload["metrics"]["compute_ms"], 42);
        }
        other => panic!("expected SemanticCacheMiss, got {other:?}"),
    }
}
