//! Host-service access through an async builtin's explicit interpreter context.

use super::NativeVmState;
use crate::vm::AsyncBuiltinCtx;
#[cfg(any(test, feature = "vm-test-support"))]
use crate::vm::Vm;
use std::sync::Arc;

#[cfg(test)]
mod tests;

pub trait NativeBuiltinCtxExt: Sized {
    fn record_evaluation_receipt(&self, receipt: crate::llm::decision::receipt::EvaluationReceipt);
    fn evaluation_invocation_id(&self) -> String;
    fn evaluation_replay(&self) -> Option<crate::llm::decision::replay::EvaluationReplayScope>;
    fn pool_registry(&self) -> Arc<crate::stdlib::pool::PoolRegistry>;
    fn package_snapshot_registry(&self) -> Arc<crate::stdlib::PackageSnapshotRegistry>;
    fn connector_client(
        &self,
        provider: &str,
    ) -> impl std::future::Future<
        Output = Result<
            Option<Arc<dyn crate::connectors::ConnectorClient>>,
            crate::connectors::ClientError,
        >,
    > + Send;
    #[cfg(any(test, feature = "vm-test-support"))]
    fn for_test(vm: Vm) -> Self;
}

impl NativeBuiltinCtxExt for AsyncBuiltinCtx {
    fn record_evaluation_receipt(&self, receipt: crate::llm::decision::receipt::EvaluationReceipt) {
        self.with_child_vm(|vm| vm.native().evaluation.journal.lock().record(receipt));
    }

    fn evaluation_invocation_id(&self) -> String {
        self.with_child_vm(|vm| {
            vm.native()
                .evaluation
                .journal
                .lock()
                .invocation_id(vm.execution_id().as_ref())
        })
    }

    fn evaluation_replay(&self) -> Option<crate::llm::decision::replay::EvaluationReplayScope> {
        self.with_child_vm(|vm| vm.native().evaluation.replay.clone())
    }

    /// Construct a context with the fixture's active registries, as the
    /// production dispatch scope does for native host work.
    #[cfg(any(test, feature = "vm-test-support"))]
    fn for_test(mut vm: Vm) -> Self {
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
    fn pool_registry(&self) -> Arc<crate::stdlib::pool::PoolRegistry> {
        self.with_child_vm(|vm| vm.native().pool_registry.clone())
    }

    fn package_snapshot_registry(&self) -> Arc<crate::stdlib::PackageSnapshotRegistry> {
        self.with_child_vm(|vm| vm.native().package_snapshot_registry.clone())
    }

    /// Resolve outside the interpreter lock so initialization can await freely.
    async fn connector_client(
        &self,
        provider: &str,
    ) -> Result<Option<Arc<dyn crate::connectors::ConnectorClient>>, crate::connectors::ClientError>
    {
        let runtime = self.with_child_vm(|vm| vm.native().connector_clients.clone());
        runtime.resolve(provider).await
    }
}
