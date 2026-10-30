//! The persistent agent-event sink a session's events are written to.

use std::path::PathBuf;
use std::sync::Arc;

use crate::event_log::AnyEventLog;

/// Where a session's durable agent events go.
///
/// The choice reads ambient state: the thread's active event log when one is
/// installed, otherwise a `HARN_EVENT_LOG_DIR` JSONL file. Preparing a VM
/// stages a lazy default event log on the thread, so a host that registers a
/// sink after preparing a VM must resolve the choice first. Otherwise
/// preparation decides where the turn's events land.
#[derive(Clone)]
pub enum DurableEventSink {
    /// The generalized event log, as the `observability.agent_events` topic.
    EventLog(Arc<AnyEventLog>),
    /// The legacy `event_log-<session>.jsonl` file under `HARN_EVENT_LOG_DIR`.
    Jsonl(PathBuf),
}

impl DurableEventSink {
    /// Resolve the sink for `session_id` without opening or registering it.
    pub fn resolve(session_id: &str) -> Option<Self> {
        if let Some(log) = crate::event_log::active_event_log() {
            return Some(Self::EventLog(log));
        }
        let dir = std::env::var("HARN_EVENT_LOG_DIR").ok()?;
        if dir.is_empty() {
            return None;
        }
        Some(Self::Jsonl(crate::run_tape::branch_path(
            PathBuf::from(dir).join(format!("event_log-{session_id}.jsonl")),
            "event log JSONL sink",
        )))
    }

    /// Open this sink and register it for `session_id`.
    ///
    /// Silent no-op on failure: a broken observability sink must never
    /// prevent a session from running.
    pub fn register(self, session_id: &str) {
        match self {
            Self::EventLog(log) => crate::agent_events::register_sink(
                session_id,
                crate::agent_events::EventLogSink::new(log, session_id),
            ),
            Self::Jsonl(path) => {
                if let Ok(sink) = crate::agent_events::JsonlEventSink::open(&path) {
                    crate::agent_events::register_sink(session_id, sink);
                }
            }
        }
    }
}

/// Resolve and register the durable sink for `session_id` now.
pub fn register_event_log_sink(session_id: &str) {
    if let Some(sink) = DurableEventSink::resolve(session_id) {
        sink.register(session_id);
    }
}
