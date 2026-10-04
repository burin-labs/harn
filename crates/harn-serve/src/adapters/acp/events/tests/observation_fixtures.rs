use harn_vm::agent_events::{session_health::SessionHealthFact, AgentEvent};

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
