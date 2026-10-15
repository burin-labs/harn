//! Native execution services carried behind the interpreter's required runtime.

use crate::value::{VmError, VmValue};
use crate::vm::{BuiltinGateDecision, Vm, VmRuntime, VmRuntimeBaseline};
use std::{any::Any, path::Path, sync::Arc};
use std::{future::Future, pin::Pin};

#[derive(Clone)]
pub(crate) struct NativeExecutionState {
    pub bridge: Option<Arc<crate::bridge::HostBridge>>,
    pub worker_registry: Arc<crate::stdlib::agents::agents_workers::WorkerRegistry>,
    pub daemon_registry: Arc<crate::stdlib::agents_daemon::DaemonRegistry>,
    pub trigger_registry: Arc<crate::triggers::registry::TriggerRegistryRuntime>,
    pub session_runtime: Arc<crate::agent_sessions::AgentSessionRuntime>,
    pub cleanup_runtimes: Option<Arc<crate::agent_lifecycle_cleanup::CleanupRuntimes>>,
    pub tracing_runtime: Arc<crate::tracing::TracingRuntime>,
    pub evaluation: crate::llm::decision::receipt::EvaluationExecutionState,
    pub agent_host_session_runtime: Arc<crate::llm::agent_session_host::AgentHostSessionRuntime>,
    pub connector_clients: Arc<crate::connectors::VmConnectorClients>,
    pub pool_registry: Arc<crate::stdlib::pool::PoolRegistry>,
    pub llm_mock_context: crate::llm::mock::LlmMockContext,
    pub package_snapshot_registry: Arc<crate::stdlib::PackageSnapshotRegistry>,
}

impl NativeExecutionState {
    pub(crate) fn fresh() -> Self {
        Self {
            bridge: None,
            worker_registry: crate::stdlib::agents::agents_workers::active_worker_registry(),
            daemon_registry: crate::stdlib::agents_daemon::active_daemon_registry(),
            trigger_registry: crate::triggers::registry::active_trigger_registry(),
            session_runtime: crate::agent_sessions::active_session_runtime(),
            cleanup_runtimes: None,
            tracing_runtime: crate::tracing::active_tracing_runtime(),
            evaluation: Default::default(),
            agent_host_session_runtime:
                crate::llm::agent_session_host::active_agent_host_session_runtime(),
            connector_clients: Arc::default(),
            pool_registry: crate::stdlib::pool::new_pool_registry(),
            llm_mock_context: crate::llm::mock::LlmMockContext::for_new_vm(),
            package_snapshot_registry: Arc::default(),
        }
    }
}

impl VmRuntime for NativeExecutionState {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn fork_child(&self) -> Arc<dyn VmRuntime> {
        Arc::new(self.clone())
    }

    fn baseline(&self) -> Arc<dyn VmRuntimeBaseline> {
        Arc::new(NativeExecutionBaseline)
    }

    fn builtin_bridge(&self) -> Option<&dyn crate::vm::VmBuiltinBridge> {
        self.bridge
            .as_deref()
            .map(|bridge| bridge as &dyn crate::vm::VmBuiltinBridge)
    }

    fn harness_methods(&self) -> &dyn crate::vm::HarnessMethods {
        crate::harness_methods::runtime_methods()
    }

    fn rebind_execution_state(&self, vm: &mut Vm) {
        crate::stdlib::rebind_execution_state_builtins(vm);
    }

    fn set_source_dir(&self, dir: &Path) {
        crate::stdlib::set_thread_source_dir(dir);
    }

    fn denied_builtin_family(
        &self,
        mut denied: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        let canonical = denied
            .iter()
            .filter_map(|name| crate::stdlib::builtin_manifest_entry(name))
            .map(|entry| entry.canonical_name)
            .collect::<std::collections::HashSet<_>>();
        if !canonical.is_empty() {
            denied.extend(
                crate::stdlib::all_builtin_manifest()
                    .iter()
                    .filter(|entry| canonical.contains(entry.canonical_name))
                    .map(|entry| entry.name.to_string()),
            );
        }
        denied
    }

    fn requires_async_builtin_gate(&self, name: &str) -> bool {
        crate::autonomy::needs_async_side_effect_enforcement(name)
    }

    fn gate_preparation(
        &self,
        name: &str,
        contract: Option<&harn_builtin_meta::BuiltinContract>,
    ) -> Result<(), VmError> {
        crate::tool_registry::preparation_scope::enforce_contract(name, contract)
    }

    fn gate_sync_builtin(&self, name: &str, args: &[VmValue]) -> Result<(), VmError> {
        crate::orchestration::enforce_current_policy_for_builtin(name, args)
    }

    fn gate_async_builtin<'a>(
        &'a self,
        name: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinGateDecision, VmError>> + Send + 'a>> {
        Box::pin(async move {
            let autonomy = if self.requires_async_builtin_gate(name) {
                crate::autonomy::enforce_builtin_side_effect_boxed(name, args).await?
            } else {
                None
            };
            if let Some(crate::autonomy::AutonomyDecision::Skip(value)) = autonomy {
                return Ok(BuiltinGateDecision::Return(value));
            }
            if !matches!(
                autonomy,
                Some(crate::autonomy::AutonomyDecision::AllowApproved)
            ) {
                crate::orchestration::enforce_current_policy_for_builtin(name, args)?;
            }
            Ok(BuiltinGateDecision::Continue)
        })
    }

    fn gate_bridge_builtin(&self, name: &str) -> Result<(), VmError> {
        crate::orchestration::enforce_current_policy_for_bridge_builtin(name)
    }

    fn builtin_effects(&self, name: &str) -> Option<&'static [harn_builtin_meta::EffectSpec]> {
        crate::stdlib::recorded_effect_builtin_manifest_entry(name)
            .map(|entry| entry.contract.effects)
    }

    fn capability_effects(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
    ) -> Option<&'static [harn_builtin_meta::EffectSpec]> {
        crate::stdlib::capability_method_manifest_entry(capability, method)
            .map(|entry| entry.contract.effects)
    }
}

struct NativeExecutionBaseline;

impl VmRuntimeBaseline for NativeExecutionBaseline {
    fn instantiate(&self, source_dir: Option<&Path>) -> Arc<dyn VmRuntime> {
        crate::initialize_runtime_assets();
        if let Some(dir) = source_dir {
            crate::stdlib::set_thread_source_dir(dir);
        }
        Arc::new(NativeExecutionState::fresh())
    }
}

pub(crate) trait NativeVmState {
    fn native(&self) -> &NativeExecutionState;
    fn native_mut(&mut self) -> &mut NativeExecutionState;
}

impl NativeVmState for Vm {
    fn native(&self) -> &NativeExecutionState {
        self.runtime()
            .as_any()
            .downcast_ref::<NativeExecutionState>()
            .expect("native VM operations require NativeExecutionState")
    }
    fn native_mut(&mut self) -> &mut NativeExecutionState {
        self.runtime_mut()
            .as_any_mut()
            .downcast_mut::<NativeExecutionState>()
            .expect("native VM operations require NativeExecutionState")
    }
}

#[cfg(test)]
mod tests;
