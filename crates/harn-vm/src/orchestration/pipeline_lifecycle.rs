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
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde_json::Value;

use crate::event_log::{EventLog, LogEvent, Topic};
use crate::value::VmClosure;

pub const LIFECYCLE_AUDIT_TOPIC: &str = "pipeline.lifecycle.audit";

thread_local! {
    static PIPELINE_ON_FINISH: RefCell<Option<Rc<VmClosure>>> = const { RefCell::new(None) };
    static LIFECYCLE_AUDIT_LOG: RefCell<Vec<LifecycleAuditEntry>> = const { RefCell::new(Vec::new()) };
    static PARTIAL_HANDOFF_REGISTRY: RefCell<Vec<PartialHandoffEnvelope>> = const { RefCell::new(Vec::new()) };
    static PIPELINE_DISPOSITION: RefCell<Option<Value>> = const { RefCell::new(None) };
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
    PIPELINE_DISPOSITION.with(|slot| *slot.borrow_mut() = None);
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
    pub pool_pending_tasks: Vec<Value>,
}

impl UnsettledStateSnapshot {
    pub fn is_empty(&self) -> bool {
        self.suspended_subagents.is_empty()
            && self.queued_triggers.is_empty()
            && self.partial_handoffs.is_empty()
            && self.in_flight_llm_calls.is_empty()
            && self.pool_pending_tasks.is_empty()
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "suspended_subagents": self.suspended_subagents,
            "queued_triggers": self.queued_triggers,
            "partial_handoffs": self.partial_handoffs,
            "in_flight_llm_calls": self.in_flight_llm_calls,
            "pool_pending_tasks": self.pool_pending_tasks,
        })
    }

    pub fn counts_json(&self) -> Value {
        serde_json::json!({
            "suspended": self.suspended_subagents.len(),
            "queued": self.queued_triggers.len(),
            "partial": self.partial_handoffs.len(),
            "in_flight": self.in_flight_llm_calls.len(),
            "pool_pending": self.pool_pending_tasks.len(),
        })
    }

    pub fn summary(&self) -> String {
        let suspended = self.suspended_subagents.len();
        let queued = self.queued_triggers.len();
        let partial = self.partial_handoffs.len();
        let in_flight = self.in_flight_llm_calls.len();
        let pool_pending = self.pool_pending_tasks.len();
        if suspended == 0 && queued == 0 && partial == 0 && in_flight == 0 && pool_pending == 0 {
            "no unsettled work".to_string()
        } else {
            format!(
                "unsettled work: {suspended} suspended subagents, {queued} queued triggers, {partial} partial handoffs, {in_flight} in-flight llm calls, {pool_pending} pool pending tasks"
            )
        }
    }
}

/// Return the current unsettled-state snapshot. This synchronous variant only
/// reads in-memory registries; VM lifecycle code should prefer
/// `unsettled_state_snapshot_async` so event-log-backed trigger queues are
/// included when available.
pub fn unsettled_state_snapshot() -> UnsettledStateSnapshot {
    unsettled_state_snapshot_base(Vec::new())
}

/// Return the current unsettled-state snapshot, including event-log-backed
/// trigger queues when an active event log is installed for this thread.
pub async fn unsettled_state_snapshot_async() -> UnsettledStateSnapshot {
    unsettled_state_snapshot_base(queued_trigger_snapshot_json().await)
}

fn unsettled_state_snapshot_base(queued_triggers: Vec<Value>) -> UnsettledStateSnapshot {
    UnsettledStateSnapshot {
        suspended_subagents: crate::stdlib::agents::snapshot_suspended_subagents(),
        queued_triggers,
        partial_handoffs: partial_handoff_snapshot_json(),
        in_flight_llm_calls: crate::llm::snapshot_in_flight_llm_calls(),
        pool_pending_tasks: crate::stdlib::pool::snapshot_pending_tasks(),
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
    persist_lifecycle_audit_entry(&entry);
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
    pub queued_at_ms: i64,
}

impl PartialHandoffEnvelope {
    pub fn to_json(&self) -> Value {
        self.to_json_at(crate::stdlib::clock::now_wall_ms())
    }

    pub fn to_json_at(&self, now_ms: i64) -> Value {
        serde_json::json!({
            "envelope_id": self.envelope_id,
            "from": self.origin_pipeline,
            "to": self.target_pipeline,
            "payload_summary": payload_summary(&self.payload),
            "queued_at_ms": self.queued_at_ms,
            "age_ms": now_ms.saturating_sub(self.queued_at_ms).max(0),
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
        queued_at_ms: crate::stdlib::clock::now_wall_ms(),
    };
    PARTIAL_HANDOFF_REGISTRY.with(|reg| reg.borrow_mut().push(envelope.clone()));
    envelope
}

/// Acknowledge a partial handoff envelope by removing it from the unsettled
/// registry and recording the settlement decision in the lifecycle audit log.
pub fn acknowledge_partial_handoff(
    envelope_id: &str,
    decision: Value,
) -> Option<PartialHandoffEnvelope> {
    let removed = PARTIAL_HANDOFF_REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        let index = reg
            .iter()
            .position(|entry| entry.envelope_id == envelope_id)?;
        Some(reg.remove(index))
    })?;
    record_lifecycle_audit(
        "handoff_acknowledged",
        serde_json::json!({
            "envelope_id": envelope_id,
            "decision": decision,
        }),
    );
    Some(removed)
}

pub fn finalize_pipeline_disposition(disposition: Value) -> Value {
    PIPELINE_DISPOSITION.with(|slot| *slot.borrow_mut() = Some(disposition.clone()));
    let entry = record_lifecycle_audit(
        "pipeline_finalized",
        serde_json::json!({
            "disposition": disposition,
        }),
    );
    serde_json::json!({
        "status": "finalized",
        "method": "finalize",
        "entry": entry.to_json(),
    })
}

pub fn pipeline_disposition_snapshot() -> Option<Value> {
    PIPELINE_DISPOSITION.with(|slot| slot.borrow().clone())
}

fn partial_handoff_snapshot_json() -> Vec<Value> {
    let now_ms = crate::stdlib::clock::now_wall_ms();
    PARTIAL_HANDOFF_REGISTRY.with(|reg| reg.borrow().iter().map(|e| e.to_json_at(now_ms)).collect())
}

fn next_seq() -> u64 {
    LIFECYCLE_SEQ.with(|seq| {
        let mut slot = seq.borrow_mut();
        *slot += 1;
        *slot
    })
}

fn persist_lifecycle_audit_entry(entry: &LifecycleAuditEntry) {
    let Some(log) = crate::event_log::active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(LIFECYCLE_AUDIT_TOPIC) else {
        return;
    };
    let mut headers = BTreeMap::new();
    headers.insert("kind".to_string(), entry.kind.clone());
    headers.insert("seq".to_string(), entry.seq.to_string());
    if let Some(pipeline_id) = entry.pipeline_id.as_ref() {
        headers.insert("pipeline_id".to_string(), pipeline_id.clone());
    }
    let _ = futures::executor::block_on(log.append(
        &topic,
        LogEvent::new("lifecycle_audit", entry.to_json()).with_headers(headers),
    ));
}

async fn queued_trigger_snapshot_json() -> Vec<Value> {
    let Some(log) = crate::event_log::active_event_log() else {
        return Vec::new();
    };
    let now_ms = lifecycle_now_ms();
    let mut out = Vec::new();
    out.extend(snapshot_inbox_triggers(log.as_ref(), now_ms).await);
    out.extend(snapshot_worker_queue_triggers(log, now_ms).await);
    out.sort_by(|left, right| {
        let left_key = (
            left.get("queued_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX),
            left.get("id").and_then(Value::as_str).unwrap_or_default(),
        );
        let right_key = (
            right
                .get("queued_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX),
            right.get("id").and_then(Value::as_str).unwrap_or_default(),
        );
        left_key.cmp(&right_key)
    });
    out
}

fn lifecycle_now_ms() -> i64 {
    crate::clock_mock::now_ms()
}

async fn snapshot_inbox_triggers(log: &crate::event_log::AnyEventLog, now_ms: i64) -> Vec<Value> {
    let Ok(inbox_topic) = Topic::new(crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC) else {
        return Vec::new();
    };
    let Ok(outbox_topic) = Topic::new(crate::triggers::TRIGGER_OUTBOX_TOPIC) else {
        return Vec::new();
    };
    let Ok(cancel_topic) = Topic::new(crate::triggers::TRIGGER_CANCEL_REQUESTS_TOPIC) else {
        return Vec::new();
    };
    let inbox = log
        .read_range(&inbox_topic, None, usize::MAX)
        .await
        .unwrap_or_default();
    let outbox = log
        .read_range(&outbox_topic, None, usize::MAX)
        .await
        .unwrap_or_default();
    let cancels = log
        .read_range(&cancel_topic, None, usize::MAX)
        .await
        .unwrap_or_default();

    let completed_events = outbox
        .into_iter()
        .filter_map(|(_, event)| {
            let event_id = event
                .headers
                .get("event_id")
                .cloned()
                .or_else(|| json_string(&event.payload, &["event_id"]))?;
            let binding_key = event
                .headers
                .get("binding_key")
                .cloned()
                .or_else(|| json_string(&event.payload, &["binding_key"]))
                .unwrap_or_default();
            Some((binding_key, event_id))
        })
        .collect::<BTreeSet<_>>();
    let cancelled_events = cancels
        .into_iter()
        .filter(|(_, event)| event.kind == "dispatch_cancel_requested")
        .filter_map(|(_, event)| {
            let event_id = json_string(&event.payload, &["event_id"])?;
            let binding_key = json_string(&event.payload, &["binding_key"]).unwrap_or_default();
            Some((binding_key, event_id))
        })
        .collect::<BTreeSet<_>>();

    inbox
        .into_iter()
        .filter(|(_, event)| event.kind == "event_ingested")
        .filter_map(|(_, event)| {
            let trigger = event
                .payload
                .get("event")
                .cloned()
                .unwrap_or_else(|| event.payload.clone());
            let event_id = json_string(&trigger, &["id"])?;
            let binding_key = event
                .headers
                .get("binding_key")
                .cloned()
                .or_else(|| {
                    let trigger_id = event
                        .headers
                        .get("trigger_id")
                        .cloned()
                        .or_else(|| json_string(&event.payload, &["trigger_id"]))?;
                    let version = json_u64(&event.payload, &["binding_version"])?;
                    Some(format!("{trigger_id}@v{version}"))
                });
            if event_is_settled(&completed_events, binding_key.as_deref(), &event_id)
                || event_is_settled(&cancelled_events, binding_key.as_deref(), &event_id)
            {
                return None;
            }
            let trigger_id = event
                .headers
                .get("trigger_id")
                .cloned()
                .or_else(|| json_string(&event.payload, &["trigger_id"]));
            let queued_at_ms = event.occurred_at_ms;
            let provider = json_string(&trigger, &["provider"]).unwrap_or_default();
            let kind = json_string(&trigger, &["kind"]).unwrap_or_default();
            let id = binding_key
                .as_ref()
                .map(|key| format!("trigger://{key}/{event_id}"))
                .unwrap_or_else(|| format!("trigger://{event_id}"));
            Some(serde_json::json!({
                "id": id,
                "event_id": event_id,
                "trigger_id": trigger_id,
                "binding_key": binding_key,
                "spec_summary": trigger_spec_summary(provider.as_str(), kind.as_str(), trigger_id.as_deref()),
                "queued_at_ms": queued_at_ms,
                "age_ms": now_ms.saturating_sub(queued_at_ms).max(0),
                "source": "trigger_inbox",
            }))
        })
        .collect()
}

fn event_is_settled(
    settled_events: &BTreeSet<(String, String)>,
    binding_key: Option<&str>,
    event_id: &str,
) -> bool {
    let scoped_key = binding_key.unwrap_or_default().to_string();
    settled_events.contains(&(scoped_key, event_id.to_string()))
        || settled_events.contains(&(String::new(), event_id.to_string()))
}

async fn snapshot_worker_queue_triggers(
    log: std::sync::Arc<crate::event_log::AnyEventLog>,
    now_ms: i64,
) -> Vec<Value> {
    let queue = crate::triggers::WorkerQueue::new(log);
    let Ok(queues) = queue.known_queues().await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for queue_name in queues {
        let Ok(state) = queue.queue_state(&queue_name).await else {
            continue;
        };
        let queue_label = state.queue.clone();
        for job in state.jobs {
            if job.acked || job.purged {
                continue;
            }
            let event_id = job.job.event.id.0.clone();
            let id = format!("worker://{}/{}", queue_label, job.job_event_id);
            out.push(serde_json::json!({
                "id": id,
                "event_id": event_id,
                "trigger_id": job.job.trigger_id,
                "binding_key": job.job.binding_key,
                "spec_summary": trigger_spec_summary(
                    job.job.event.provider.0.as_str(),
                    job.job.event.kind.as_str(),
                    Some(job.job.trigger_id.as_str())
                ),
                "queued_at_ms": job.enqueued_at_ms,
                "age_ms": now_ms.saturating_sub(job.enqueued_at_ms).max(0),
                "source": "worker_queue",
                "queue": queue_label.clone(),
                "job_event_id": job.job_event_id,
                "claimed": job.active_claim.is_some(),
            }));
        }
    }
    out
}

fn trigger_spec_summary(provider: &str, kind: &str, trigger_id: Option<&str>) -> String {
    match (
        trigger_id.filter(|id| !id.is_empty()),
        provider.is_empty(),
        kind.is_empty(),
    ) {
        (Some(id), false, false) => format!("{id}: {provider}.{kind}"),
        (Some(id), _, _) => id.to_string(),
        (None, false, false) => format!("{provider}.{kind}"),
        (None, false, true) => provider.to_string(),
        (None, true, false) => kind.to_string(),
        (None, true, true) => "trigger event".to_string(),
    }
}

fn json_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_str().map(ToString::to_string)
}

fn json_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_u64()
}

fn payload_summary(payload: &Value) -> String {
    match payload {
        Value::Null => "nil".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => {
            let mut chars = value.chars();
            let preview: String = chars.by_ref().take(80).collect();
            if chars.next().is_some() {
                format!("{preview}...")
            } else {
                preview
            }
        }
        Value::Array(items) => format!("list(len={})", items.len()),
        Value::Object(map) => {
            let keys = map.keys().take(6).cloned().collect::<Vec<_>>().join(",");
            if map.len() > 6 {
                format!("object(keys={keys},...)")
            } else {
                format!("object(keys={keys})")
            }
        }
    }
}
