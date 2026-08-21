use harn_vm::agent_events::{
    AgentEvent, FinalWrapupToolCallParseStatus, FinalWrapupUnconsumedToolCall,
};

pub(super) fn events() -> Vec<AgentEvent> {
    vec![
        AgentEvent::RequireSuccessfulToolsViolation {
            session_id: "session-1".to_string(),
            kind: "tool_gap".to_string(),
            source: "agent_loop.require_successful_tools".to_string(),
            actor: Some("implementer".to_string()),
            run_id: Some("run-1".to_string()),
            redacted_summary: "agent_loop completed without invoking edit".to_string(),
            recurrence_hints: vec!["missing_required_tools=1".to_string()],
            metadata: serde_json::json!({
                "missing_required_tools": ["edit"],
                "successful_tool_names": [],
                "iterations": 2,
            }),
        },
        AgentEvent::FinalWrapup {
            session_id: "session-1".to_string(),
            final_status: "max_iterations".to_string(),
            stop_reason: "iteration_limit".to_string(),
            iteration: 4,
            host_directive: false,
            terminal_kind: "max_iterations".to_string(),
            unconsumed_tool_call: Some(FinalWrapupUnconsumedToolCall {
                parse_status: FinalWrapupToolCallParseStatus::Parsed,
                parsed_call_count: 1,
                tool_names: vec!["edit".to_string()],
                diagnostics: Vec::new(),
                evidence_line: "the final summary contained an unconsumed tool call".to_string(),
            }),
        },
        AgentEvent::PackThinkingStripped {
            session_id: "session-1".to_string(),
            model: "claude-opus-adaptive".to_string(),
            requested: "high".to_string(),
            reason: "claude_opus_adaptive".to_string(),
        },
        AgentEvent::SelfConsistencyTie {
            session_id: "session-1".to_string(),
            answer: "alpha".to_string(),
            total: 4,
            distribution: serde_json::json!([
                {"answer": "alpha", "count": 2},
                {"answer": "beta", "count": 2},
            ]),
        },
        AgentEvent::CodeLibrarianQueryNlFallback {
            session_id: "session-1".to_string(),
            attempted_cypher: None,
            mcts_depth: 3,
            mcts_expansions: 9,
            result_count: 2,
            text: "where is session recovery implemented?".to_string(),
        },
        AgentEvent::ModelJob {
            session_id: "session-1".to_string(),
            event: serde_json::json!({
                "schema": "harn.model_job_event.v1",
                "kind": "state_changed",
                "job_id": "job-1",
                "request_id": "logo-1",
                "backend": "fixture",
                "state": "running",
                "at_ms": 12
            }),
        },
    ]
}
