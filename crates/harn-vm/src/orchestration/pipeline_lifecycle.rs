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

use std::cell::RefCell;
use std::rc::Rc;

use crate::value::VmClosure;

thread_local! {
    static PIPELINE_ON_FINISH: RefCell<Option<Rc<VmClosure>>> = const { RefCell::new(None) };
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

/// Drop any pending callback. Called from `reset_thread_local_state` so test
/// harnesses don't carry registrations across runs.
pub fn clear_pipeline_on_finish() {
    PIPELINE_ON_FINISH.with(|slot| *slot.borrow_mut() = None);
}

/// Snapshot of unsettled work that the pipeline `on_finish` harness exposes.
///
/// Buckets intentionally stay JSON-shaped at this boundary: each producer
/// owns its richer Rust types, while callbacks need a stable Harn dict/list
/// contract. Producers without a durable per-item registry yet return a typed
/// empty list rather than inventing storage in the lifecycle layer.
#[derive(Debug, Default, Clone)]
pub struct UnsettledStateSnapshot {
    pub suspended_subagents: Vec<serde_json::Value>,
    pub queued_triggers: Vec<serde_json::Value>,
    pub partial_handoffs: Vec<serde_json::Value>,
    pub in_flight_llm_calls: Vec<serde_json::Value>,
}

impl UnsettledStateSnapshot {
    pub fn is_empty(&self) -> bool {
        self.suspended_subagents.is_empty()
            && self.queued_triggers.is_empty()
            && self.partial_handoffs.is_empty()
            && self.in_flight_llm_calls.is_empty()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "suspended_subagents": self.suspended_subagents,
            "queued_triggers": self.queued_triggers,
            "partial_handoffs": self.partial_handoffs,
            "in_flight_llm_calls": self.in_flight_llm_calls,
        })
    }

    pub fn counts_json(&self) -> serde_json::Value {
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
        partial_handoffs: Vec::new(),
        in_flight_llm_calls: crate::llm::snapshot_in_flight_llm_calls(),
    }
}
