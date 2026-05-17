//! Run-event sink: an in-process bus the CLI installs to capture every
//! observable side effect of a `harn run` invocation as a single
//! ordered stream.
//!
//! Concrete sinks live in `harn-cli` (`harn run --json` writes them as
//! NDJSON). The VM only knows it should call [`emit`] from a handful of
//! observability checkpoints; sinks fan-out from there.
//!
//! Variants intentionally mirror the surface area of the run command
//! rather than the on-disk event log. Stdout/stderr writes are captured
//! here because they never enter the event log; transcript / persona /
//! hook / tool events are forwarded here in addition to their existing
//! persistent topics so a single subscriber can see the whole run
//! without joining across topics.

use std::sync::{Arc, OnceLock, RwLock};

use serde::Serialize;

/// One observable event from a running pipeline. Variants are
/// `#[serde(tag = "event_type")]` so wire consumers (notably the CLI
/// `--json` NDJSON stream) can discriminate without inspecting the
/// payload shape.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum RunEvent {
    /// Bytes written to stdout (raw, including any trailing newlines).
    Stdout { payload: String },
    /// Bytes written to stderr (raw, including any trailing newlines).
    Stderr { payload: String },
    /// One append on a transcript stream (`agent.transcript.llm` topic).
    /// `kind` mirrors the transcript entry's `type` field.
    Transcript {
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        kind: String,
        payload: serde_json::Value,
    },
    /// A model-issued tool call. `call_id` matches the transcript
    /// `call_id`; agents reconcile [`Self::ToolCall`] /
    /// [`Self::ToolResult`] pairs by it.
    ToolCall {
        call_id: String,
        name: String,
        args: serde_json::Value,
        /// RFC 3339 timestamp captured at emission.
        started_at: String,
    },
    /// Outcome of a tool call.
    ToolResult {
        call_id: String,
        ok: bool,
        result: serde_json::Value,
    },
    /// A workflow hook fired during the run.
    Hook {
        name: String,
        phase: String,
        #[serde(skip_serializing_if = "serde_json::Value::is_null")]
        payload: serde_json::Value,
    },
    /// A persona-stage transition. Mirrors the `persona.runtime.events`
    /// topic; the `transition` field captures the stage state change
    /// (`"started"`, `"completed"`, `"handoff"`, ...).
    PersonaStage {
        persona: String,
        stage: String,
        transition: String,
    },
}

/// Receiver of [`RunEvent`]s. Implementations must be cheap (the VM
/// calls [`emit`] on hot paths like every `println`).
pub trait RunEventSink: Send + Sync {
    fn emit(&self, event: RunEvent);
}

fn active_slot() -> &'static RwLock<Option<Arc<dyn RunEventSink>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<dyn RunEventSink>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install `sink` as the process-wide run-event sink. Returns the
/// previous sink (if any) so callers can chain. Installs are
/// exclusive — there is one active sink at a time — to keep ordering
/// trivially serial.
pub fn install_sink(sink: Arc<dyn RunEventSink>) -> Option<Arc<dyn RunEventSink>> {
    let mut guard = active_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.replace(sink)
}

/// Remove the active sink. No-op when none is installed.
pub fn clear_sink() {
    let mut guard = active_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

/// Whether a sink is currently installed. Useful as a fast-path gate
/// for callers that would otherwise build a payload speculatively.
pub fn sink_active() -> bool {
    active_slot()
        .read()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Emit `event` to the active sink. No-op when no sink is installed,
/// so it is safe to call from every hook point unconditionally.
pub fn emit(event: RunEvent) {
    let guard = match active_slot().read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(sink) = guard.as_ref() {
        sink.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The sink slot is process-global; tests that install/clear must
    /// run serially even when nextest fans them out across threads.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct CapturingSink {
        events: Mutex<Vec<RunEvent>>,
    }

    impl RunEventSink for CapturingSink {
        fn emit(&self, event: RunEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn install_emit_clear_round_trip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sink = Arc::new(CapturingSink {
            events: Mutex::new(Vec::new()),
        });
        let prior = install_sink(sink.clone());
        assert!(sink_active());
        emit(RunEvent::Stdout {
            payload: "hi\n".into(),
        });
        clear_sink();
        assert!(!sink_active());
        emit(RunEvent::Stdout {
            payload: "after-clear".into(),
        });

        let captured = sink.events.lock().unwrap();
        assert_eq!(captured.len(), 1, "events after clear must be dropped");
        match &captured[0] {
            RunEvent::Stdout { payload } => assert_eq!(payload, "hi\n"),
            other => panic!("unexpected event {other:?}"),
        }

        if let Some(prior) = prior {
            install_sink(prior);
        }
    }
}
