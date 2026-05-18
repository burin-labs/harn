//! Pipeline-finish lifecycle state.
//!
//! The pipeline DSL accepts a single `on_finish` callback that runs after the
//! pipeline's declared steps complete but before the pipeline returns. The
//! callback receives `(harness, return_value)` and may transform the value.
//! Storage is a thread-local one-shot slot: `Vm::execute` consumes the
//! registered closure with `take_pipeline_on_finish` exactly once, so a stale
//! registration cannot leak across consecutive runs.
//!
//! `unsettled_state_snapshot` exposes the pipeline-finish harness view of
//! work that can outlive the main pipeline body.
//!
//! Beyond the snapshot, drain callbacks need two write-side surfaces:
//! `record_lifecycle_audit` (which `harness.emit_audit` routes to) and
//! `record_partial_handoff` (which `harness.handoff_to` routes to). Both are
//! thread-local because pipeline execution is single-threaded per run and we
//! want deterministic ordering for replay/conformance.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value;

use crate::value::VmClosure;

thread_local! {
    static PIPELINE_ON_FINISH: RefCell<Option<Rc<VmClosure>>> = const { RefCell::new(None) };
    static LIFECYCLE_AUDIT_LOG: RefCell<Vec<LifecycleAuditEntry>> = const { RefCell::new(Vec::new()) };
    static PARTIAL_HANDOFF_REGISTRY: RefCell<Vec<PartialHandoffEnvelope>> = const { RefCell::new(Vec::new()) };
    static LIFECYCLE_SEQ: RefCell<u64> = const { RefCell::new(0) };
}

/// Register the callback `Vm::execute` will invoke after the pipeline's
/// declared steps complete. Last-write-wins.
pub fn set_pipeline_on_finish(callback: Rc<VmClosure>) {
    PIPELINE_ON_FINISH.with(|slot| *slot.borrow_mut() = Some(callback));
}

/// Consume the pending callback, leaving the slot empty. Returns `None` when
/// no callback was registered.
pub fn take_pipeline_on_finish() -> Option<Rc<VmClosure>> {
    PIPELINE_ON_FINISH.with(|slot| slot.borrow_mut().take())
}

/// Drop any pending callback and every captured lifecycle audit entry,
/// partial-handoff envelope, and seq counter. Called from
/// `reset_thread_local_state` so test harnesses don't carry registrations
/// across runs, and from `Vm::execute` on the error exit path so a
/// failed pipeline doesn't leak in-progress lifecycle state into the
/// next run.
pub fn clear_pipeline_on_finish() {
    PIPELINE_ON_FINISH.with(|slot| *slot.borrow_mut() = None);
    LIFECYCLE_AUDIT_LOG.with(|log| log.borrow_mut().clear());
    PARTIAL_HANDOFF_REGISTRY.with(|reg| reg.borrow_mut().clear());
    LIFECYCLE_SEQ.with(|seq| *seq.borrow_mut() = 0);
}

/// Snapshot of unsettled work that the pipeline `on_finish` harness exposes.
///
/// Buckets intentionally stay JSON-shaped at this boundary: each producer
/// owns its richer Rust types, while callbacks need a stable Harn dict/list
/// contract. Producers without a durable per-item registry yet return a typed
/// empty list rather than inventing storage in the lifecycle layer.
#[derive(Debug, Default, Clone)]
pub struct UnsettledStateSnapshot {
    pub suspended_subagents: Vec<Value>,
    pub queued_triggers: Vec<Value>,
    pub partial_handoffs: Vec<Value>,
    pub in_flight_llm_calls: Vec<Value>,
}

impl UnsettledStateSnapshot {
    pub fn is_empty(&self) -> bool {
        self.suspended_subagents.is_empty()
            && self.queued_triggers.is_empty()
            && self.partial_handoffs.is_empty()
            && self.in_flight_llm_calls.is_empty()
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "suspended_subagents": self.suspended_subagents,
            "queued_triggers": self.queued_triggers,
            "partial_handoffs": self.partial_handoffs,
            "in_flight_llm_calls": self.in_flight_llm_calls,
        })
    }

    pub fn counts_json(&self) -> Value {
        serde_json::json!({
            "suspended": self.suspended_subagents.len(),
            "queued": self.queued_triggers.len(),
            "partial": self.partial_handoffs.len(),
            "in_flight": self.in_flight_llm_calls.len(),
        })
    }

    pub fn summary(&self) -> String {
        let suspended = self.suspended_subagents.len();
        let queued = self.queued_triggers.len();
        let partial = self.partial_handoffs.len();
        let in_flight = self.in_flight_llm_calls.len();
        if suspended == 0 && queued == 0 && partial == 0 && in_flight == 0 {
            "no unsettled work".to_string()
        } else {
            format!(
                "unsettled work: {suspended} suspended subagents, {queued} queued triggers, {partial} partial handoffs, {in_flight} in-flight llm calls"
            )
        }
    }
}

/// Return the current unsettled-state snapshot. This is a single synchronous
/// collection point for all currently available per-thread registries.
pub fn unsettled_state_snapshot() -> UnsettledStateSnapshot {
    UnsettledStateSnapshot {
        suspended_subagents: crate::stdlib::agents::snapshot_suspended_subagents(),
        queued_triggers: Vec::new(),
        partial_handoffs: partial_handoff_snapshot_json(),
        in_flight_llm_calls: crate::llm::snapshot_in_flight_llm_calls(),
    }
}

/// One recorded `harness.emit_audit` call. `seq` is a per-pipeline-run
/// monotonic counter so conformance fixtures and replay can match entries by
/// shape rather than wall-clock time.
#[derive(Debug, Clone)]
pub struct LifecycleAuditEntry {
    pub seq: u64,
    pub kind: String,
    pub payload: Value,
    pub pipeline_id: Option<String>,
}

impl LifecycleAuditEntry {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "seq": self.seq,
            "kind": self.kind,
            "payload": self.payload,
            "pipeline_id": self.pipeline_id,
        })
    }
}

/// Record an audit entry from a lifecycle callback. Returns the entry's seq
/// number so the caller can echo it back as a receipt.
pub fn record_lifecycle_audit(kind: impl Into<String>, payload: Value) -> LifecycleAuditEntry {
    let entry = LifecycleAuditEntry {
        seq: next_seq(),
        kind: kind.into(),
        payload,
        pipeline_id: crate::orchestration::current_mutation_session()
            .and_then(|session| session.run_id.or(Some(session.session_id))),
    };
    LIFECYCLE_AUDIT_LOG.with(|log| log.borrow_mut().push(entry.clone()));
    entry
}

/// Drain the audit log, returning all entries recorded since the last drain.
/// Used by conformance fixtures and replay oracles.
pub fn take_lifecycle_audit_log() -> Vec<LifecycleAuditEntry> {
    LIFECYCLE_AUDIT_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// Non-destructive read of the audit log for introspection.
pub fn lifecycle_audit_log_snapshot() -> Vec<LifecycleAuditEntry> {
    LIFECYCLE_AUDIT_LOG.with(|log| log.borrow().clone())
}

/// One recorded `harness.handoff_to` call. The runtime stores envelopes in
/// the thread-local registry so a follow-on pipeline (or the conformance
/// suite) can inspect them via `unsettled_state_snapshot()`.
#[derive(Debug, Clone)]
pub struct PartialHandoffEnvelope {
    pub envelope_id: String,
    pub target_pipeline: String,
    pub origin_pipeline: Option<String>,
    pub payload: Value,
    pub seq: u64,
}

impl PartialHandoffEnvelope {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "envelope_id": self.envelope_id,
            "target_pipeline": self.target_pipeline,
            "origin_pipeline": self.origin_pipeline,
            "payload": self.payload,
            "seq": self.seq,
        })
    }
}

/// Record a partial-handoff envelope and return the entry. Allocates a
/// deterministic envelope id derived from the lifecycle seq counter so test
/// fixtures don't depend on wall-clock time or uuids.
pub fn record_partial_handoff(
    target_pipeline: impl Into<String>,
    payload: Value,
) -> PartialHandoffEnvelope {
    let seq = next_seq();
    let envelope = PartialHandoffEnvelope {
        envelope_id: format!("envelope_{seq}"),
        target_pipeline: target_pipeline.into(),
        origin_pipeline: crate::orchestration::current_mutation_session()
            .and_then(|session| session.run_id.or(Some(session.session_id))),
        payload,
        seq,
    };
    PARTIAL_HANDOFF_REGISTRY.with(|reg| reg.borrow_mut().push(envelope.clone()));
    envelope
}

fn partial_handoff_snapshot_json() -> Vec<Value> {
    PARTIAL_HANDOFF_REGISTRY.with(|reg| reg.borrow().iter().map(|e| e.to_json()).collect())
}

fn next_seq() -> u64 {
    LIFECYCLE_SEQ.with(|seq| {
        let mut slot = seq.borrow_mut();
        *slot += 1;
        *slot
    })
}
