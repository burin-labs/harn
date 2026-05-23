//! Subagent ↔ DAP thread bridging (issue #1868, epic #1836).
//!
//! Each spawned worker becomes a DAP `Thread`; suspended workers surface
//! as `Stopped(reason="suspend")`; resumed workers emit DAP `continued`.
//! This file owns the queue + mapping; emission lives in
//! [`Debugger::drain_subagent_events`] in [`super::state`] / [`super::events`].
//!
//! ## Wire model
//!
//! - Worker ↔ DAP thread id is 1:1. Ids start at 100 to keep human-
//!   readable separation from the synthetic `main` (1) and any ACP
//!   session threads (2..).
//! - `parent_worker_id` propagates onto the DAP `Thread.parentId`
//!   extension so IDEs can render lineage trees.
//! - The full lifecycle map is:
//!
//!   | `WorkerEvent`         | DAP emission                                   |
//!   | --------------------- | ---------------------------------------------- |
//!   | `WorkerSpawned`       | `thread { reason: "started" }`                 |
//!   | `WorkerProgressed`    | (no DAP event — too noisy)                     |
//!   | `WorkerWaitingForInput` | `stopped { reason: "pause", description }`   |
//!   | `WorkerSuspended`     | `stopped { reason: "suspend", description }`   |
//!   | `WorkerResumed`       | `continued { allThreadsContinued: false }`     |
//!   | `WorkerCompleted`     | `thread { reason: "exited" }`                  |
//!   | `WorkerFailed`        | `stopped { reason: "exception" }` + `thread exited` |
//!   | `WorkerCancelled`     | `thread { reason: "exited" }`                  |
//!
//! ## Privacy
//!
//! DAP receives the worker identity, lineage, lifecycle status, and the
//! structured suspension envelope needed for debugger inspection. It
//! deliberately does not receive transcripts, resume input, or arbitrary
//! worker metadata blobs.
//!
//! ## Filters
//!
//! Three exception-breakpoint filters in [`crate::protocol::Capabilities`]
//! control whether suspend/resume/drain events surface as a forced
//! `stopped` versus an ambient `thread` event:
//!
//! - `break-on-suspend`: when active, a `WorkerSuspended` event raises a
//!   stop on the subagent thread. When inactive, the ambient `stopped`
//!   event still fires (the DAP spec models the suspended runtime state
//!   as a Stopped thread, regardless of whether the IDE has paused the
//!   *debugger session* itself).
//! - `break-on-resume`: when active, a `WorkerResumed` event causes the
//!   adapter to additionally pause the *main* debugger thread so the
//!   user can step through the resume continuation.
//! - `break-on-drain-decision`: registered for parity with the issue
//!   spec; surfaces ambient events but does not currently force a stop
//!   because drain decisions flow through the lifecycle-hook path
//!   rather than `AgentEvent` (P-03 wiring is the consumer). Documented
//!   as a known limitation; tests cover registration / no-op behaviour.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use harn_vm::agent_events::{AgentEvent, AgentEventSink, WildcardSinkHandle, WorkerEvent};
use serde_json::Value;

/// Stable thread ids for subagent threads start at this base. Keeps
/// them well above the synthetic `main` (1) and the ACP session range
/// (2..), so a `threads` response is unambiguous to a human reader.
pub(crate) const SUBAGENT_THREAD_ID_BASE: u64 = 100;

/// Per-subagent record kept on the [`SubagentTracker`].
#[derive(Clone, Debug)]
pub(crate) struct SubagentThread {
    pub(crate) thread_id: u64,
    pub(crate) name: String,
    pub(crate) parent_worker_id: Option<String>,
    pub(crate) last_status: String,
    /// Most-recent suspend reason. Cleared on resume / completion so
    /// the next stop description doesn't echo a stale reason.
    pub(crate) suspend_reason: Option<String>,
    /// Structured suspension envelope exposed through DAP scopes when
    /// the suspended subagent thread is selected.
    pub(crate) suspension: Option<Value>,
    /// True after `thread { reason: "exited" }` has been emitted; the
    /// record sticks around briefly so a `threads` response that races
    /// with the exit can still surface the row, but no further events
    /// will be emitted for it.
    pub(crate) exited: bool,
}

/// Queue payload — one DAP-relevant subagent lifecycle observation.
/// The sink runs inside the VM's tokio task; the debugger drains the
/// queue between VM steps from the main message loop.
#[derive(Clone, Debug)]
pub(crate) struct SubagentObservation {
    pub(crate) worker_id: String,
    pub(crate) worker_name: String,
    pub(crate) event: WorkerEvent,
    pub(crate) status: String,
    pub(crate) parent_worker_id: Option<String>,
    pub(crate) suspend_reason: Option<String>,
    pub(crate) suspension: Option<Value>,
}

/// Shared state between the [`SubagentEventSink`] (which mutates from
/// the VM's runtime) and the [`crate::debugger::Debugger`] (which reads
/// from the main thread). Holds the queue plus the worker_id → thread
/// id map so id allocation is consistent across the two surfaces.
#[derive(Debug, Default)]
pub(crate) struct SubagentTrackerInner {
    pub(crate) pending: Vec<SubagentObservation>,
    pub(crate) threads: BTreeMap<String, SubagentThread>,
    pub(crate) next_thread_id: u64,
}

impl SubagentTrackerInner {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            threads: BTreeMap::new(),
            next_thread_id: SUBAGENT_THREAD_ID_BASE,
        }
    }
}

/// Cheap-to-clone handle around the shared subagent state.
#[derive(Clone, Debug)]
pub(crate) struct SubagentTracker {
    inner: Arc<Mutex<SubagentTrackerInner>>,
}

impl SubagentTracker {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SubagentTrackerInner::new())),
        }
    }

    /// Drain every pending observation. The caller (the debugger main
    /// loop) translates them into DAP events.
    pub(crate) fn drain(&self) -> Vec<SubagentObservation> {
        let mut guard = self.inner.lock().expect("subagent tracker poisoned");
        std::mem::take(&mut guard.pending)
    }

    /// Allocate (or look up) the DAP thread id for `worker_id`. Records
    /// the worker name + parent so subsequent `threads` requests can
    /// rebuild the lineage without re-scanning observations.
    pub(crate) fn upsert_thread(
        &self,
        worker_id: &str,
        worker_name: &str,
        parent_worker_id: Option<&str>,
        status: &str,
    ) -> SubagentThread {
        let mut guard = self.inner.lock().expect("subagent tracker poisoned");
        if let Some(existing) = guard.threads.get_mut(worker_id) {
            existing.last_status = status.to_string();
            // Names occasionally update on first re-emission (the
            // initial WorkerSpawned arrives before the persona name is
            // resolved); keep the freshest one.
            if !worker_name.is_empty() {
                existing.name = worker_name.to_string();
            }
            if parent_worker_id.is_some() {
                existing.parent_worker_id = parent_worker_id.map(str::to_string);
            }
            existing.clone()
        } else {
            let id = guard.next_thread_id;
            guard.next_thread_id += 1;
            let record = SubagentThread {
                thread_id: id,
                name: worker_name.to_string(),
                parent_worker_id: parent_worker_id.map(str::to_string),
                last_status: status.to_string(),
                suspend_reason: None,
                suspension: None,
                exited: false,
            };
            guard.threads.insert(worker_id.to_string(), record.clone());
            record
        }
    }

    /// Mark a subagent's record as suspended, attaching the envelope
    /// shown in the DAP variable inspector.
    pub(crate) fn mark_suspended(
        &self,
        worker_id: &str,
        reason: Option<&str>,
        suspension: Option<Value>,
    ) {
        let mut guard = self.inner.lock().expect("subagent tracker poisoned");
        if let Some(thread) = guard.threads.get_mut(worker_id) {
            thread.last_status = "suspended".to_string();
            thread.suspend_reason = reason.map(str::to_string);
            thread.suspension = suspension;
        }
    }

    /// Mark a subagent's record as resumed. Clears any stored suspend
    /// reason so a later `threads` snapshot doesn't lie about why a
    /// running worker is parked.
    pub(crate) fn mark_resumed(&self, worker_id: &str) {
        let mut guard = self.inner.lock().expect("subagent tracker poisoned");
        if let Some(thread) = guard.threads.get_mut(worker_id) {
            thread.last_status = "running".to_string();
            thread.suspend_reason = None;
            thread.suspension = None;
        }
    }

    /// Mark a subagent's record as exited. The record stays in the map
    /// for one more `threads` snapshot (so an IDE that races with the
    /// `thread exited` event still sees the row) but flags itself so
    /// subsequent emissions become no-ops.
    pub(crate) fn mark_exited(&self, worker_id: &str) {
        let mut guard = self.inner.lock().expect("subagent tracker poisoned");
        if let Some(thread) = guard.threads.get_mut(worker_id) {
            thread.exited = true;
            thread.suspend_reason = None;
            thread.suspension = None;
        }
    }

    /// Snapshot every known subagent thread for a `threads` response.
    /// Returns `(worker_id, SubagentThread)` so the caller can resolve
    /// parent links by worker id without a second lookup. Sweeps
    /// records flagged as exited after snapshotting so the registry
    /// doesn't grow unboundedly across long-lived sessions.
    pub(crate) fn snapshot_threads(&self) -> Vec<(String, SubagentThread)> {
        let mut guard = self.inner.lock().expect("subagent tracker poisoned");
        let snapshot: Vec<(String, SubagentThread)> = guard
            .threads
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        guard.threads.retain(|_, t| !t.exited);
        snapshot
    }

    /// Look up the DAP thread id assigned to a worker. Reserved for
    /// future routing — host code that wants to e.g. emit a
    /// `stackTrace` request scoped to a specific subagent can resolve
    /// the id without re-walking the snapshot. Currently exercised in
    /// tests; the `threads` response path uses `snapshot_threads`'s
    /// `(worker_id, thread)` tuples for lookup so it can pair parent
    /// lineage in a single pass.
    #[allow(dead_code)]
    pub(crate) fn thread_id_for_worker(&self, worker_id: &str) -> Option<u64> {
        let guard = self.inner.lock().expect("subagent tracker poisoned");
        guard.threads.get(worker_id).map(|t| t.thread_id)
    }

    pub(crate) fn thread_for_id(&self, thread_id: u64) -> Option<SubagentThread> {
        let guard = self.inner.lock().expect("subagent tracker poisoned");
        guard
            .threads
            .values()
            .find(|thread| thread.thread_id == thread_id)
            .cloned()
    }

    /// Test helper — peek at the queue length without draining.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }

    /// Test helper — borrow the inner state for direct queue
    /// manipulation. The DAP tests rely on this to seed observations
    /// without round-tripping through the wildcard sink registry,
    /// which would otherwise leak across the process-global sink
    /// store.
    #[cfg(test)]
    pub(crate) fn inner_for_test(&self) -> std::sync::MutexGuard<'_, SubagentTrackerInner> {
        self.inner.lock().expect("subagent tracker poisoned")
    }
}

/// `AgentEventSink` impl that captures `WorkerUpdate` events into the
/// tracker. Registered as a wildcard sink so it observes every session
/// the debugger's VM might open; other event variants are dropped on
/// the floor because the DAP path only models worker lineage.
pub(crate) struct SubagentEventSink {
    tracker: SubagentTracker,
}

impl SubagentEventSink {
    pub(crate) fn new(tracker: SubagentTracker) -> Self {
        Self { tracker }
    }
}

impl AgentEventSink for SubagentEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        let AgentEvent::WorkerUpdate {
            worker_id,
            worker_name,
            event,
            status,
            metadata,
            ..
        } = event
        else {
            return;
        };
        let parent_worker_id = metadata
            .get("parent_worker_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let suspension = metadata
            .get("suspension")
            .filter(|value| value.is_object())
            .cloned();
        let suspend_reason = metadata
            .get("suspension")
            .and_then(|v| v.get("reason"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let observation = SubagentObservation {
            worker_id: worker_id.clone(),
            worker_name: worker_name.clone(),
            event: *event,
            status: status.clone(),
            parent_worker_id,
            suspend_reason,
            suspension,
        };
        let mut guard = self
            .tracker
            .inner
            .lock()
            .expect("subagent tracker poisoned");
        guard.pending.push(observation);
    }
}

/// Handle for the wildcard sink registration; unregistered on drop so
/// the debugger never leaks a dangling sink past a session.
pub(crate) struct SubagentSinkRegistration {
    handle: WildcardSinkHandle,
}

impl SubagentSinkRegistration {
    pub(crate) fn install(tracker: SubagentTracker) -> Self {
        let sink: Arc<dyn AgentEventSink> = Arc::new(SubagentEventSink::new(tracker));
        let handle = harn_vm::agent_events::register_wildcard_sink(sink);
        Self { handle }
    }
}

impl Drop for SubagentSinkRegistration {
    fn drop(&mut self) {
        harn_vm::agent_events::unregister_wildcard_sink(self.handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_update(
        worker_id: &str,
        worker_name: &str,
        event: WorkerEvent,
        parent: Option<&str>,
        suspend_reason: Option<&str>,
    ) -> AgentEvent {
        let mut metadata = serde_json::Map::new();
        if let Some(p) = parent {
            metadata.insert("parent_worker_id".to_string(), json!(p));
        }
        if let Some(r) = suspend_reason {
            metadata.insert(
                "suspension".to_string(),
                json!({
                    "handle": worker_id,
                    "reason": r,
                    "conditions": {
                        "trigger": {"provider": "github", "kind": "comment"},
                        "timeout": {"minutes": 30},
                    },
                    "resume_by_mechanism": "trigger",
                    "suspended_at": "01978f25-0000-7000-8000-000000000000",
                    "initiator": "operator",
                }),
            );
        }
        AgentEvent::WorkerUpdate {
            session_id: "session-test".to_string(),
            worker_id: worker_id.to_string(),
            worker_name: worker_name.to_string(),
            worker_task: "task".to_string(),
            worker_mode: "mode".to_string(),
            event,
            status: event.as_status().to_string(),
            metadata: serde_json::Value::Object(metadata),
            audit: None,
        }
    }

    #[allow(dead_code)]
    fn _construct_with_mode_field_compiles() {
        // Belt-and-braces: the AgentEvent::WorkerUpdate shape includes
        // `worker_mode` even though our sink ignores it. Constructing
        // here ensures we won't silently miss a field-rename upstream.
        let _ = make_update("w", "n", WorkerEvent::WorkerSpawned, None, None);
    }

    #[test]
    fn tracker_drain_returns_pending_then_clears() {
        let tracker = SubagentTracker::new();
        let sink = SubagentEventSink::new(tracker.clone());
        sink.handle_event(&make_update(
            "w1",
            "alpha",
            WorkerEvent::WorkerSpawned,
            None,
            None,
        ));
        sink.handle_event(&make_update(
            "w1",
            "alpha",
            WorkerEvent::WorkerSuspended,
            None,
            Some("budget"),
        ));
        assert_eq!(tracker.pending_len(), 2);
        let drained = tracker.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(tracker.pending_len(), 0);
        assert_eq!(drained[0].event, WorkerEvent::WorkerSpawned);
        assert_eq!(drained[1].event, WorkerEvent::WorkerSuspended);
        assert_eq!(drained[1].suspend_reason.as_deref(), Some("budget"));
        assert_eq!(
            drained[1].suspension.as_ref().unwrap()["resume_by_mechanism"],
            "trigger"
        );
    }

    #[test]
    fn upsert_thread_allocates_unique_ids_per_worker() {
        let tracker = SubagentTracker::new();
        let a = tracker.upsert_thread("w1", "alpha", None, "running");
        let b = tracker.upsert_thread("w2", "beta", Some("w1"), "running");
        assert_ne!(a.thread_id, b.thread_id);
        assert!(a.thread_id >= SUBAGENT_THREAD_ID_BASE);
        assert!(b.thread_id >= SUBAGENT_THREAD_ID_BASE);
        assert_eq!(b.parent_worker_id.as_deref(), Some("w1"));
        // Re-upsert is idempotent (same id).
        let a2 = tracker.upsert_thread("w1", "alpha", None, "suspended");
        assert_eq!(a.thread_id, a2.thread_id);
        assert_eq!(a2.last_status, "suspended");
    }

    #[test]
    fn mark_resumed_clears_suspend_reason() {
        let tracker = SubagentTracker::new();
        tracker.upsert_thread("w1", "alpha", None, "running");
        tracker.mark_suspended("w1", Some("backoff"), Some(json!({"reason": "backoff"})));
        let snapshot = tracker.snapshot_threads();
        assert_eq!(snapshot[0].1.suspend_reason.as_deref(), Some("backoff"));
        tracker.mark_resumed("w1");
        let snapshot = tracker.snapshot_threads();
        assert_eq!(snapshot[0].1.suspend_reason, None);
        assert_eq!(snapshot[0].1.last_status, "running");
    }

    #[test]
    fn snapshot_threads_sweeps_exited_after_emission() {
        let tracker = SubagentTracker::new();
        tracker.upsert_thread("w1", "alpha", None, "running");
        tracker.upsert_thread("w2", "beta", None, "running");
        tracker.mark_exited("w1");
        let first = tracker.snapshot_threads();
        // Exited worker still appears in the first snapshot so a racing
        // `threads` request after the `thread exited` event still has
        // the row to render.
        assert_eq!(first.len(), 2);
        let second = tracker.snapshot_threads();
        assert_eq!(second.len(), 1, "exited row swept after first snapshot");
        assert_eq!(second[0].1.name, "beta");
    }

    #[test]
    fn thread_id_for_worker_resolves_parent_lineage() {
        let tracker = SubagentTracker::new();
        let parent = tracker.upsert_thread("w1", "alpha", None, "running");
        let child = tracker.upsert_thread("w2", "beta", Some("w1"), "running");
        assert_eq!(tracker.thread_id_for_worker("w1"), Some(parent.thread_id));
        assert_eq!(tracker.thread_id_for_worker("w2"), Some(child.thread_id));
        assert_eq!(tracker.thread_id_for_worker("nonexistent"), None);
    }

    #[test]
    fn sink_skips_non_worker_events() {
        let tracker = SubagentTracker::new();
        let sink = SubagentEventSink::new(tracker.clone());
        sink.handle_event(&AgentEvent::TurnStart {
            session_id: "s".to_string(),
            iteration: 1,
            provider: String::new(),
            model: String::new(),
        });
        assert_eq!(tracker.pending_len(), 0);
    }
}
