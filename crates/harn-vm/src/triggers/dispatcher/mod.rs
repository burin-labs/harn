//! Trigger dispatcher: routes inbox envelopes to bindings, applies retry/flow control,
//! and emits trust + lifecycle events.
//!
//! This file is wiring only — module declarations, the shared imports every
//! submodule globs in via `use super::*`, and the crate-facing re-exports. The
//! dispatch path itself reads top to bottom across the submodules:
//!
//! - `lifecycle` — build a dispatcher, enqueue onto the inbox, run/drain/shutdown.
//! - `routing` — resolve an envelope or event to bindings; span + metrics wrapper.
//! - `admission` — cancel requests, `when` predicate, budgets, autonomy ceiling.
//! - `flow_gates` — batch/debounce/rate-limit/throttle/singleton/concurrency.
//! - `attempt_loop` — the attempt/retry/DLQ state machine for one binding.
//! - `handlers` — route one attempt to its destination, with `spawn_to_pool`,
//!   `reminder_inject`, `interrupt_and_suspend`, and `persona` as siblings.
//!
//! Support code lives in `types`, `state`, `util`, `audit`, `action_graph`,
//! `vm_invoke`, `circuits`, `flow_control`, `predicate_eval`, `retry`, `uri`.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{pin_mut, StreamExt};
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use tracing::Instrument as _;

use crate::event_log::{active_event_log, AnyEventLog, EventLog, LogEvent, Topic};
use crate::llm::vm_value_to_json;
use crate::orchestration::{
    RunActionGraphEdgeRecord, RunActionGraphNodeRecord, ACTION_GRAPH_EDGE_KIND_DLQ_MOVE,
    ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN, ACTION_GRAPH_EDGE_KIND_RETRY,
    ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH, ACTION_GRAPH_NODE_KIND_DLQ,
    ACTION_GRAPH_NODE_KIND_PREDICATE, ACTION_GRAPH_NODE_KIND_RETRY, ACTION_GRAPH_NODE_KIND_TRIGGER,
};
use crate::trust_graph::{AutonomyTier, TrustOutcome};
use crate::vm::Vm;

use self::uri::DispatchUri;
use super::registry::{
    binding_autonomy_budget_would_exceed, binding_budget_would_exceed, matching_bindings,
    micros_to_usd, note_autonomous_decision, note_binding_budget_cost,
    note_orchestrator_budget_cost, orchestrator_budget_would_exceed, AgentScope, TargetExpr,
    TriggerBinding, TriggerBudgetExhaustionStrategy, TriggerHandlerSpec,
};
use super::{
    begin_in_flight, finish_in_flight, TriggerDispatchOutcome, TriggerEvent,
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_CANCEL_REQUESTS_TOPIC,
    TRIGGER_DLQ_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_OBSERVABILITY_TOPIC,
    TRIGGER_OUTBOX_TOPIC,
};
use circuits::{
    destination_circuit_key, dlq_node_metadata, DestinationCircuitProbe,
    DESTINATION_CIRCUIT_FAILURE_THRESHOLD,
};
use flow_control::BatchDecision;
use predicate_eval::predicate_node_metadata;

mod action_graph;
mod admission;
mod attempt_loop;
mod audit;
mod circuits;
mod flow_control;
mod flow_gates;
mod handlers;
mod interrupt_and_suspend;
mod lifecycle;
mod persona;
mod predicate_eval;
mod reminder_inject;
pub mod retry;
mod routing;
mod spawn_to_pool;
pub(crate) mod state;
mod types;
pub mod uri;
mod util;
mod vm_invoke;

pub use retry::{RetryPolicy, TriggerRetryConfig, DEFAULT_MAX_ATTEMPTS};
pub use types::{
    DispatchAttemptRecord, DispatchCancelRequest, DispatchError, DispatchOutcome, DispatchStatus,
    Dispatcher, DispatcherDrainReport, DispatcherStatsSnapshot, DlqEntry, InboxEnvelope,
};
pub(crate) use util::append_trigger_inbox_envelope;
pub use util::{
    append_dispatch_cancel_request, clear_dispatcher_state, enqueue_trigger_event,
    snapshot_dispatcher_stats,
};

/// Public wrapper for [`util::build_batched_event`]. Used by
/// `crate::channels` (CH-04 / #1875) to package aggregated events into a
/// single [`TriggerEvent`] before dispatch.
pub fn build_batched_event_public(
    events: Vec<TriggerEvent>,
) -> Result<TriggerEvent, DispatchError> {
    util::build_batched_event(events)
}

pub(crate) use state::{
    current_dispatch_context, current_dispatch_is_replay, current_dispatch_wait_lease, is_replay,
};
pub(crate) use types::{DispatchContext, DispatchWaitLease, TriggerInboxTopicScope};

use action_graph::{
    dispatch_entry_edge_kind, dispatch_error_label, dispatch_error_metadata, dispatch_node_id,
    dispatch_node_kind, dispatch_node_label, dispatch_node_metadata, dispatch_success_metadata,
    dispatch_success_outcome, dispatch_target_agent, retry_node_metadata,
    trigger_event_persona_metadata, trigger_node_metadata,
};
#[cfg(test)]
use state::install_test_inbox_dequeued_signal;
use state::{notify_test_inbox_dequeued, ACTIVE_DISPATCHER_STATE, ACTIVE_DISPATCH_IS_REPLAY};
use types::{
    AcquiredFlowControl, ConcurrencyLease, DispatchCallResult, DispatchSkipStage,
    DispatcherRuntimeState, FlowControlOutcome, SingletonLease, DEFAULT_AUTONOMY_BUDGET_REVIEWER,
};
use util::{
    accepted_at_ms, binding_key_from_parts, build_batched_event, cancelled_dispatch_outcome,
    current_unix_ms, decrement_in_flight, decrement_retry_queue_depth, dispatch_cancel_requested,
    dispatch_result_cost_usd_micros, dispatch_result_status, duration_between_ms, event_headers,
    extract_event_path, extracted_key_value, extracted_priority_value, json_value_to_gate,
    maybe_fail_before_outbox, now_rfc3339, now_unix_ms, queue_appended_at_ms, recv_cancel,
    sleep_or_cancel_or_request, tenant_id, unix_ms, worker_queue_priority,
};

pub const TRIGGER_ACCEPTED_AT_MS_HEADER: &str = "harn_trigger_accepted_at_ms";
pub const TRIGGER_NORMALIZED_AT_MS_HEADER: &str = "harn_trigger_normalized_at_ms";
pub const TRIGGER_QUEUE_APPENDED_AT_MS_HEADER: &str = "harn_trigger_queue_appended_at_ms";

#[cfg(test)]
mod tests;
