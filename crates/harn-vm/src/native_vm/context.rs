//! Host-service access through an async builtin's explicit interpreter context.

use super::NativeVmState;
use crate::vm::AsyncBuiltinCtx;
#[cfg(test)]
use crate::vm::Vm;
use std::sync::Arc;

#[cfg(test)]
mod tests;

impl AsyncBuiltinCtx {
    pub(crate) fn record_evaluation_receipt(
        &self,
        receipt: crate::llm::decision::receipt::EvaluationReceipt,
    ) {
        self.with_child_vm(|vm| vm.native().evaluation.journal.lock().record(receipt));
    }

    pub(crate) fn evaluation_invocation_id(&self) -> String {
        self.with_child_vm(|vm| {
            vm.native()
                .evaluation
                .journal
                .lock()
                .invocation_id(vm.execution_id().as_ref())
        })
    }

    pub(crate) fn evaluation_replay(
        &self,
    ) -> Option<crate::llm::decision::replay::EvaluationReplayScope> {
        self.with_child_vm(|vm| vm.native().evaluation.replay.clone())
    }

    /// Construct a context with the fixture's active registries, as the
    /// production dispatch scope does for native host work.
    #[cfg(test)]
    pub fn for_test(mut vm: Vm) -> Self {
        let native = vm.native_mut();
        native.worker_registry = crate::stdlib::agents::agents_workers::active_worker_registry();
        native.daemon_registry = crate::stdlib::agents_daemon::active_daemon_registry();
        native.trigger_registry = crate::triggers::registry::active_trigger_registry();
        native.session_runtime = crate::agent_sessions::active_session_runtime();
        native.tracing_runtime = crate::tracing::active_tracing_runtime();
        native.agent_host_session_runtime =
            crate::llm::agent_session_host::active_agent_host_session_runtime();
        Self::from_vm(vm)
    }

    /// Pool lookup follows the execution tree across Tokio worker threads.
    pub(crate) fn pool_registry(&self) -> Arc<crate::stdlib::pool::PoolRegistry> {
        self.with_child_vm(|vm| vm.native().pool_registry.clone())
    }

    pub(crate) fn package_snapshot_registry(&self) -> Arc<crate::stdlib::PackageSnapshotRegistry> {
        self.with_child_vm(|vm| vm.native().package_snapshot_registry.clone())
    }

    /// Resolve outside the interpreter lock so initialization can await freely.
    pub(crate) async fn connector_client(
        &self,
        provider: &str,
    ) -> Result<Option<Arc<dyn crate::connectors::ConnectorClient>>, crate::connectors::ClientError>
    {
        let runtime = self.with_child_vm(|vm| vm.native().connector_clients.clone());
        runtime.resolve(provider).await
    }
}
