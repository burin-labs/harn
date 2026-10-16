use harn_vm::agent_events::{session_health::SessionHealthFact, AgentEvent};

pub(super) fn fixture_judge_decision() -> AgentEvent {
    AgentEvent::JudgeDecision {
        session_id: "session-1".to_string(),
        iteration: 0,
        verdict: "continue".to_string(),
        reasoning: "needs one more concrete action".to_string(),
        next_step: Some("run the verifier".to_string()),
        judge_duration_ms: 42,
        source: Some("deterministic".to_string()),
        trigger: Some("stalled".to_string()),
        reason: Some("repeated_verification_failures".to_string()),
        confirm: Some(false),
        converted_from: Some("cosmetic_only".to_string()),
        escalation_recommended: Some(true),
        escalation_target: Some("frontier".to_string()),
        judge_verdict: None,
        judge_override: None,
        gap_class: None,
        judge_outcome: None,
        terminal_evidence_preserved: None,
        requirement_report: Some(serde_json::json!({
            "schema": "harn.completion_requirement_report.v1",
            "complete": false,
            "required_count": 1,
            "reported_count": 1,
            "pending_count": 1,
            "pending_names": ["Reach the requested handler."],
            "rows": [{
                "requirement_id": "behavior",
                "name": "Reach the requested handler.",
                "met": false,
                "evidence_refs": [9],
                "status": "unsupported",
                "evidence_roles": ["deterministic_verification"]
            }]
        })),
    }
}

pub(super) fn fixture_watchdog() -> AgentEvent {
    AgentEvent::DaemonWatchdogTripped {
        session_id: "session-1".to_string(),
        attempts: 5,
        elapsed_ms: 12_000,
    }
}

pub(super) fn fixture_session_health() -> AgentEvent {
    AgentEvent::SessionHealth {
        session_id: "session-1".to_string(),
        fact: Box::new(SessionHealthFact {
            schema_version: 1,
            session_id: "session-1".to_string(),
            iteration: None,
            turn: Default::default(),
            rolling: Default::default(),
            heuristics: Default::default(),
        }),
    }
}

pub(super) fn fixture_tool_format_override() -> AgentEvent {
    AgentEvent::ToolFormatOverride {
        session_id: "session-1".to_string(),
        provider: "openrouter".to_string(),
        model: "qwen/qwen3-coder".to_string(),
        requested_format: "native".to_string(),
        recommended_format: "text".to_string(),
        catalog_parity: "native_unreliable".to_string(),
        override_reason: Some("cross-check provider regression".to_string()),
        applied_format: Some("text".to_string()),
        steered: Some(true),
    }
}
