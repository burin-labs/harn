//! Pipeline-finish lifecycle state.
//!
//! The pipeline DSL accepts a single `on_finish` callback that runs after the
//! pipeline's declared steps complete but before the pipeline returns. The
//! callback receives `(harness, return_value)` and may transform the value.
//! Storage is a thread-local one-shot slot: `Vm::execute` consumes the
//! registered closure with `take_pipeline_on_finish` exactly once, so a stale
//! registration cannot leak across consecutive runs.
//!
//! P-04 will populate `unsettled_state_snapshot` with suspended subagents,
//! queued triggers, partial handoffs, and in-flight LLM calls. For P-01 the
//! snapshot is always empty and `OnUnsettledDetected` therefore never fires.

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
/// P-04 populates the four buckets; P-01 ships an always-empty snapshot so
/// the lifecycle wiring is exercised without depending on suspend/resume,
/// reactive triggers, handoff envelopes, or in-flight LLM tracking.
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
}

/// Return the current unsettled-state snapshot. Always empty until P-04
/// wires in the per-bucket producers.
pub fn unsettled_state_snapshot() -> UnsettledStateSnapshot {
    UnsettledStateSnapshot::default()
}
