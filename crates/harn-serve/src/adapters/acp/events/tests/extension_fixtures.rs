//! One fixture event per Harn session-update extension, in schema order.

use super::*;

pub(super) fn extension_fixture_events() -> Vec<AgentEvent> {
    vec![
        AgentEvent::InferenceSpeed {
            session_id: "session-1".to_string(),
            call_id: "llm-call-1".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-5-5".to_string(),
            speed: harn_vm::llm::inference_speed::InferenceSpeed {
                phase: harn_vm::llm::inference_speed::InferencePhase::Complete,
                is_final: true,
                elapsed_ms: 4210,
                time_to_first_token_ms: Some(812),
                output_tokens: 431,
                output_tokens_per_second: Some(126.8),
                token_source: harn_vm::llm::inference_speed::TokenCountSource::Provider,
            },
        },
        AgentEvent::Artifact {
            session_id: "session-1".to_string(),
            artifact_id: "artifact-chart-1".to_string(),
            kind: "vega-lite".to_string(),
            title: Some("Build throughput".to_string()),
            mime_type: "application/vnd.vegalite.v5+json".to_string(),
            spec: serde_json::json!({
                "mark": "bar",
                "data": {"values": [{"name": "a", "count": 2}]},
                "encoding": {
                    "x": {"field": "name", "type": "nominal"},
                    "y": {"field": "count", "type": "quantitative"}
                }
            }),
            fallback: "Build throughput (bar chart)".to_string(),
            size_bytes: 153,
            provenance: serde_json::json!({"source": "agent"}),
            metadata: serde_json::json!({"unit": "builds"}),
        },
        AgentEvent::SkillActivated {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            iteration: 1,
            reason: "matched".to_string(),
        },
        AgentEvent::SkillDeactivated {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            iteration: 2,
        },
        AgentEvent::SkillScopeTools {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            allowed_tools: vec!["read".to_string()],
        },
        AgentEvent::SkillNarrow {
            session_id: "session-1".to_string(),
            reason: "unused across 5 turns".to_string(),
            removed_tools: vec!["write".to_string()],
            remaining_tools: vec!["read".to_string()],
            policy: serde_json::Value::Null,
            removed_tool_details: serde_json::Value::Null,
            kept_tool_details: serde_json::Value::Null,
        },
        AgentEvent::StanceTransition {
            session_id: "session-1".to_string(),
            phase: "write_access_granted".to_string(),
            escape_tool: "request_write_access".to_string(),
            allowed_tools: vec![
                "look".to_string(),
                "search".to_string(),
                "request_write_access".to_string(),
            ],
            justification: "User asked me to make the change.".to_string(),
            consent: "express".to_string(),
            reason: "The user explicitly asked for the edit.".to_string(),
        },
        AgentEvent::ToolSearchQuery {
            session_id: "session-1".to_string(),
            tool_use_id: "search-1".to_string(),
            name: "tool_search".to_string(),
            query: serde_json::json!({"q": "read"}),
            strategy: "semantic".to_string(),
            mode: "client".to_string(),
        },
        AgentEvent::ToolSearchResult {
            session_id: "session-1".to_string(),
            tool_use_id: "search-1".to_string(),
            promoted: vec!["read".to_string()],
            strategy: "semantic".to_string(),
            mode: "client".to_string(),
        },
        AgentEvent::TranscriptCompacted {
            session_id: "session-1".to_string(),
            receipt: fixture_compaction_receipt(
                "compaction-fixture-1",
                Some("extend"),
                Some("host"),
                Some(serde_json::json!({
                    "instructions": "Preserve failing tests.",
                    "instruction_mode": "extend",
                    "instruction_source": "host"
                })),
            ),
        },
        AgentEvent::TranscriptProjected {
            session_id: "session-1".to_string(),
            policy: "clean_tool_repair".to_string(),
            reason: "tool_call_repair_squashed".to_string(),
            prefix_hash: "sha256:abc".to_string(),
            kept_count: 3,
            dropped_count: 2,
            provider_safety_blocked: false,
            redacted_count: 0,
            reclaimed_tokens: 0,
            roots_consulted: Vec::new(),
            redaction_pointers: Vec::new(),
        },
        AgentEvent::ReminderEmitted {
            session_id: "session-1".to_string(),
            reminder_id: "reminder-1".to_string(),
            tags: vec!["token_pressure".to_string()],
            body: "Refresh the compacted context before answering.".to_string(),
            role_hint: "developer".to_string(),
            authority: "contract".to_string(),
            rendered_role: "developer".to_string(),
            source: "stdlib_provider".to_string(),
            ttl_turns: Some(2),
        },
        AgentEvent::Handoff {
            session_id: "session-1".to_string(),
            artifact_id: "artifact-1".to_string(),
            handoff: Box::new(fixture_handoff()),
        },
        AgentEvent::FsWatch {
            session_id: "session-1".to_string(),
            subscription_id: "fsw-1".to_string(),
            events: vec![FsWatchEvent {
                kind: "modify".to_string(),
                paths: vec!["/tmp/project/src/lib.rs".to_string()],
                relative_paths: vec!["src/lib.rs".to_string()],
                raw_kind: "Modify(Any)".to_string(),
                error: None,
            }],
        },
        test_support::staged_writes_fixture_event(),
        AgentEvent::WorkerUpdate {
            session_id: "session-1".into(),
            worker_id: "worker-1".into(),
            worker_name: "review".into(),
            worker_task: "review pr".into(),
            worker_mode: "delegated_stage".into(),
            event: harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
            status: "awaiting_input".into(),
            metadata: serde_json::json!({
                "child_run_id": "run_x",
                "child_run_path": ".harn-runs/run_x",
            }),
            audit: Some(serde_json::json!({"run_id": "run_x"})),
        },
        AgentEvent::HitlRequested {
            session_id: "session-1".into(),
            request_id: "hitl_question_session-1_1".into(),
            kind: "question".into(),
            payload: serde_json::json!({"prompt": "Approve deploy?"}),
        },
        AgentEvent::HitlResolved {
            session_id: "session-1".into(),
            request_id: "hitl_question_session-1_1".into(),
            kind: "question".into(),
            outcome: "answered".into(),
        },
    ]
}
