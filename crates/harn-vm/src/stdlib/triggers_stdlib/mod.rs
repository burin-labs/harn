//! Harn-facing trigger surface: the `std/triggers` builtins.
//!
//! Wiring only. Each submodule owns one family of the surface:
//!
//! - `dispatch` — register/list/fire/replay a trigger, and the test harness.
//! - `journal` — the event-log record shapes plus the `trigger_inspect_*` reads.
//! - `binding_config` — parsing the `trigger_register` config dict.
//! - `event_input` — building a [`TriggerEvent`] from a hand-written dict.
//! - `trust` / `corrections` — the trust-graph and correction-record builtins.
//! - `webhook_intake` — registering signed webhook endpoints and feeding them.
//! - `auto_resume` — the private triggers that resume suspended workers.
//! - `args` — shared argument and dict-field accessors.

use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::VmBuiltinDef;
use crate::vm::Vm;

mod args;
mod auto_resume;
mod binding_config;
mod corrections;
mod dispatch;
mod event_input;
mod journal;
mod trust;
mod webhook_intake;

pub(crate) use auto_resume::{
    register_auto_resume_trigger, reset_auto_resume_timeouts, unregister_auto_resume_trigger,
    AutoResumeTriggerHandle,
};
pub(crate) use binding_config::validate_resume_trigger_spec;

const ACTION_GRAPH_TOPIC: &str = "observability.action_graph";
const TRIGGER_EVENTS_TOPIC: &str = "triggers.events";
const TRIGGER_EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;

pub(crate) fn register_trigger_builtins(vm: &mut Vm) {
    trust::register_trust_namespace(vm);
    corrections::register_corrections_namespace(vm);

    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &dispatch::HANDLER_CONTEXT_IMPL_DEF,
    &dispatch::LIST_PROVIDERS_NATIVE_IMPL_DEF,
    &dispatch::TRIGGER_LIST_IMPL_DEF,
    &dispatch::TRIGGER_REGISTER_IMPL_DEF,
    &dispatch::TRIGGER_FIRE_IMPL_DEF,
    &dispatch::TRIGGER_REPLAY_IMPL_DEF,
    &dispatch::TRIGGER_TEST_HARNESS_IMPL_DEF,
    &journal::TRIGGER_INSPECT_DLQ_IMPL_DEF,
    &journal::TRIGGER_INSPECT_LIFECYCLE_IMPL_DEF,
    &journal::TRIGGER_INSPECT_ACTION_GRAPH_IMPL_DEF,
    &trust::TRUST_RECORD_IMPL_DEF,
    &trust::TRUST_GRAPH_RECORD_IMPL_DEF,
    &trust::TRUST_QUERY_IMPL_DEF,
    &trust::TRUST_GRAPH_QUERY_IMPL_DEF,
    &trust::TRUST_GRAPH_POLICY_FOR_IMPL_DEF,
    &trust::TRUST_GRAPH_VERIFY_CHAIN_IMPL_DEF,
    &trust::TRUST_QUERY_NS_IMPL_DEF,
    &trust::TRUST_RECORD_NS_IMPL_DEF,
    &trust::TRUST_SCORE_NS_IMPL_DEF,
    &trust::TRUST_POLICY_FOR_NS_IMPL_DEF,
    &trust::TRUST_VERIFY_CHAIN_NS_IMPL_DEF,
    &corrections::CORRECTION_RECORD_IMPL_DEF,
    &corrections::CORRECTION_QUERY_IMPL_DEF,
    &corrections::CORRECTIONS_RECORD_NS_IMPL_DEF,
    &corrections::CORRECTIONS_QUERY_NS_IMPL_DEF,
    &webhook_intake::WEBHOOK_INTAKE_REGISTER_IMPL_DEF,
    &webhook_intake::WEBHOOK_INTAKE_FEED_IMPL_DEF,
    &webhook_intake::WEBHOOK_INTAKE_DEREGISTER_IMPL_DEF,
    &webhook_intake::WEBHOOK_INTAKE_LIST_IMPL_DEF,
    &webhook_intake::WEBHOOK_INTAKE_RECENT_IMPL_DEF,
];

#[cfg(test)]
mod tests;
