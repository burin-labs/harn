use harn_vm::agent_events::{AgentEvent, StagedWriteSummary};

pub(super) const STAGED_WRITES_META_FIELDS: &[&str] = &[
    "phase",
    "message",
    "progress",
    "kind",
    "pendingCount",
    "totalBytes",
    "pendingWrites",
];

pub(super) fn staged_writes_fixture_event() -> AgentEvent {
    AgentEvent::StagedWritesPending {
        session_id: "session-1".to_string(),
        pending_count: 1,
        total_bytes: 7,
        pending_writes: vec![StagedWriteSummary {
            path: "/tmp/project/src/lib.rs".to_string(),
            kind: "modify".to_string(),
            byte_delta: 3,
            snapshot_id: Some("tool-42".to_string()),
        }],
    }
}

pub(super) fn update_harn_meta(payload: &serde_json::Value) -> &serde_json::Value {
    &payload["params"]["update"]["_meta"]["harn"]
}
