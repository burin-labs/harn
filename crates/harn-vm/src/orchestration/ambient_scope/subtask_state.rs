//! Ambient state inherited specifically by child-interpreter subtasks.

use std::sync::Arc;

use super::clone_via_swap;
use crate::call_budget::CallBudget;
use crate::event_log::AnyEventLog;
use crate::redact::NamedPattern;
use crate::security::SecurityPolicy;
use crate::vm::subtask::{swap_subtask_placement_context, SubtaskPlacement};

/// One structural contract for state that must cross a subtask thread boundary.
///
/// Absence is part of each slot's explicit contract: for example, `None`
/// means no dispatch call budget or event log is installed. Keeping the slots
/// together prevents a new child-interpreter path from capturing only a subset
/// of the security, audit, placement, and shared-budget state.
#[derive(Clone, Default)]
pub(super) struct SubtaskAmbientState {
    placement: Option<SubtaskPlacement>,
    explicit_egress_policy_depth: usize,
    ssrf_guard_depth: usize,
    security_policy: Vec<SecurityPolicy>,
    redaction_patterns: Vec<NamedPattern>,
    event_log: Option<Arc<AnyEventLog>>,
    mcp_call_budget: Option<CallBudget>,
    pg_query_budget: Option<CallBudget>,
    tool_call_cancellations: Arc<crate::tool_call_cancellations::CancellationRegistry>,
    worker_registry: Arc<crate::stdlib::agents::agents_workers::WorkerRegistry>,
    daemon_registry: Arc<crate::stdlib::agents_daemon::DaemonRegistry>,
    trigger_registry: Arc<crate::triggers::registry::TriggerRegistryRuntime>,
    session_runtime: Arc<crate::agent_sessions::AgentSessionRuntime>,
    tracing_runtime: Arc<crate::tracing::TracingRuntime>,
    agent_host_session_runtime: Arc<crate::llm::agent_session_host::AgentHostSessionRuntime>,
}

impl SubtaskAmbientState {
    pub(super) fn capture() -> Self {
        Self {
            placement: clone_via_swap(swap_subtask_placement_context),
            explicit_egress_policy_depth: clone_via_swap(
                crate::egress::swap_require_explicit_egress_policy_depth,
            ),
            ssrf_guard_depth: clone_via_swap(crate::egress::swap_require_ssrf_guard_depth),
            security_policy: clone_via_swap(crate::security::swap_security_policy_stack),
            redaction_patterns: clone_via_swap(crate::redact::swap_custom_patterns),
            event_log: clone_via_swap(crate::event_log::swap_active_event_log),
            mcp_call_budget: clone_via_swap(crate::call_budget::swap_mcp_call_budget),
            pg_query_budget: clone_via_swap(crate::call_budget::swap_pg_query_budget),
            tool_call_cancellations: clone_via_swap(
                crate::tool_call_cancellations::swap_active_registry,
            ),
            worker_registry: clone_via_swap(
                crate::stdlib::agents::agents_workers::swap_active_worker_registry,
            ),
            daemon_registry: clone_via_swap(
                crate::stdlib::agents_daemon::swap_active_daemon_registry,
            ),
            trigger_registry: clone_via_swap(
                crate::triggers::registry::swap_active_trigger_registry,
            ),
            session_runtime: clone_via_swap(crate::agent_sessions::swap_active_session_runtime),
            tracing_runtime: clone_via_swap(crate::tracing::swap_active_tracing_runtime),
            agent_host_session_runtime: clone_via_swap(
                crate::llm::agent_session_host::swap_active_agent_host_session_runtime,
            ),
        }
    }

    pub(super) fn set_placement(&mut self, placement: Option<SubtaskPlacement>) {
        self.placement = placement;
    }

    pub(super) fn set_tool_call_cancellations(
        &mut self,
        registry: Arc<crate::tool_call_cancellations::CancellationRegistry>,
    ) {
        self.tool_call_cancellations = registry;
    }

    pub(super) fn set_worker_registry(
        &mut self,
        registry: Arc<crate::stdlib::agents::agents_workers::WorkerRegistry>,
    ) {
        self.worker_registry = registry;
    }

    pub(super) fn set_daemon_registry(
        &mut self,
        registry: Arc<crate::stdlib::agents_daemon::DaemonRegistry>,
    ) {
        self.daemon_registry = registry;
    }

    pub(super) fn set_trigger_registry(
        &mut self,
        registry: Arc<crate::triggers::registry::TriggerRegistryRuntime>,
    ) {
        self.trigger_registry = registry;
    }

    pub(super) fn set_session_runtime(
        &mut self,
        runtime: Arc<crate::agent_sessions::AgentSessionRuntime>,
    ) {
        self.session_runtime = runtime;
    }

    pub(super) fn set_tracing_runtime(&mut self, runtime: Arc<crate::tracing::TracingRuntime>) {
        self.tracing_runtime = runtime;
    }

    pub(super) fn set_agent_host_session_runtime(
        &mut self,
        runtime: Arc<crate::llm::agent_session_host::AgentHostSessionRuntime>,
    ) {
        self.agent_host_session_runtime = runtime;
    }

    pub(super) fn swap_in_place(&mut self) {
        fn swap_slot<T: Default>(slot: &mut T, swap: impl FnOnce(T) -> T) {
            *slot = swap(std::mem::take(slot));
        }

        swap_slot(&mut self.placement, swap_subtask_placement_context);
        swap_slot(
            &mut self.explicit_egress_policy_depth,
            crate::egress::swap_require_explicit_egress_policy_depth,
        );
        swap_slot(
            &mut self.ssrf_guard_depth,
            crate::egress::swap_require_ssrf_guard_depth,
        );
        swap_slot(
            &mut self.security_policy,
            crate::security::swap_security_policy_stack,
        );
        swap_slot(
            &mut self.redaction_patterns,
            crate::redact::swap_custom_patterns,
        );
        swap_slot(&mut self.event_log, crate::event_log::swap_active_event_log);
        swap_slot(
            &mut self.mcp_call_budget,
            crate::call_budget::swap_mcp_call_budget,
        );
        swap_slot(
            &mut self.pg_query_budget,
            crate::call_budget::swap_pg_query_budget,
        );
        swap_slot(
            &mut self.tool_call_cancellations,
            crate::tool_call_cancellations::swap_active_registry,
        );
        swap_slot(
            &mut self.worker_registry,
            crate::stdlib::agents::agents_workers::swap_active_worker_registry,
        );
        swap_slot(
            &mut self.daemon_registry,
            crate::stdlib::agents_daemon::swap_active_daemon_registry,
        );
        swap_slot(
            &mut self.trigger_registry,
            crate::triggers::registry::swap_active_trigger_registry,
        );
        swap_slot(
            &mut self.session_runtime,
            crate::agent_sessions::swap_active_session_runtime,
        );
        swap_slot(
            &mut self.tracing_runtime,
            crate::tracing::swap_active_tracing_runtime,
        );
        swap_slot(
            &mut self.agent_host_session_runtime,
            crate::llm::agent_session_host::swap_active_agent_host_session_runtime,
        );
    }
}
