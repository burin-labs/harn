use std::sync::Arc;

use super::{AmbientExecutionScope, LlmMockContext};

impl AmbientExecutionScope {
    /// Capture the full ambient context and typed owners for one top-level VM
    /// run. Interleaved executions cannot observe each other's state.
    pub(crate) fn capture_for_top_level_execution(
        owner: Arc<str>,
        llm_mock: LlmMockContext,
        worker_registry: Arc<crate::stdlib::agents::agents_workers::WorkerRegistry>,
        daemon_registry: Arc<crate::stdlib::agents_daemon::DaemonRegistry>,
        trigger_registry: Arc<crate::triggers::registry::TriggerRegistryRuntime>,
        session_runtime: Arc<crate::agent_sessions::AgentSessionRuntime>,
        tracing_runtime: Arc<crate::tracing::TracingRuntime>,
        agent_host_session_runtime: Arc<crate::llm::agent_session_host::AgentHostSessionRuntime>,
    ) -> Self {
        let mut scope = Self::capture_for_inline_subtask();
        let cancellations = scope
            .host_bridge
            .as_ref()
            .map_or_else(crate::tool_call_cancellations::fresh_registry, |bridge| {
                bridge.tool_call_cancellation_registry()
            });
        scope.subtask.set_tool_call_cancellations(cancellations);
        scope.subtask.set_worker_registry(worker_registry);
        scope.subtask.set_daemon_registry(daemon_registry);
        scope.subtask.set_trigger_registry(trigger_registry);
        scope.subtask.set_session_runtime(session_runtime);
        scope.subtask.set_tracing_runtime(tracing_runtime);
        scope
            .subtask
            .set_agent_host_session_runtime(agent_host_session_runtime);
        scope.execution_scope.push(owner);
        scope.llm_mock = llm_mock;
        scope
    }
}
