//! Native execution services carried behind the interpreter's required runtime.

use crate::value::{VmError, VmValue};
use crate::vm::{BuiltinGateDecision, Vm, VmRuntime, VmRuntimeBaseline};
use std::{any::Any, path::Path, sync::Arc};
use std::{future::Future, pin::Pin};

mod bridge;
mod context;
pub use context::NativeBuiltinCtxExt;
mod execution_evidence;
mod lifecycle;
use lifecycle::NativeVmLifecycle;
mod stdlib_registration;
mod step_hooks;
mod step_lifecycle;
pub(crate) use stdlib_registration::install_shared_stdlib_registration;
pub(crate) mod subtask;
mod task_cleanup;
#[cfg(test)]
pub(crate) use task_cleanup::NativeCleanupTestExt;
mod timed_execution;
mod tracing;

#[derive(Clone)]
pub struct NativeExecutionState {
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

    fn prepare_top_level(&mut self, execution_id: &crate::ExecutionId, new_execution: bool) {
        if new_execution {
            // Receipts from one run must never appear in the next run's record.
            crate::llm::reset_reasoning_receipts();
            self.evaluation.journal = Arc::default();
        }
        self.cleanup_runtimes = Some(Arc::new(
            self.current_cleanup_runtimes(execution_id)
                .capture_transport(),
        ));
    }

    fn top_level_scope(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> Box<dyn crate::vm::VmExecutionScope> {
        Box::new(NativeExecutionScope {
            ambient: self.capture_top_level_ambient(execution_id),
            pools: Arc::clone(&self.pool_registry),
        })
    }

    fn task_cleanup(&self, execution_id: &crate::ExecutionId) -> Arc<dyn crate::vm::VmTaskCleanup> {
        self.retained_cleanup_runtimes(execution_id)
    }

    fn subtask_scope(&self) -> Box<dyn crate::vm::VmExecutionScope> {
        subtask::capture_inline(Arc::clone(&self.pool_registry))
    }

    fn poll_async_builtin(
        &self,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        crate::stdlib::pool::with_pool_registry_poll(&self.pool_registry, || continuation(cx))
    }

    fn execute_timed<'a>(
        &'a self,
        vm: &'a mut Vm,
        entry: crate::vm::TopLevelEntry,
        deadline: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(timed_execution::execute(vm, entry, deadline))
    }

    fn poll_source_dir(
        &self,
        source_dir: &mut Option<std::path::PathBuf>,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        crate::orchestration::poll_source_dir(source_dir, || continuation(cx))
    }

    fn start_span(&self, kind: crate::vm::VmSpanKind, name: String) -> crate::vm::ScopeSpan {
        tracing::start_span(kind, name)
    }

    fn builtin_span(&self, name: &str) -> Option<crate::vm::ScopeSpan> {
        tracing::builtin_span(name)
    }

    fn finish_pipeline<'a>(
        &'a self,
        vm: &'a mut Vm,
        value: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(vm.run_pipeline_finish_lifecycle(value))
    }

    fn clear_pipeline_finish(&self) {
        crate::orchestration::clear_pipeline_on_finish();
    }

    fn rebind_execution_state(&self, vm: &mut Vm) {
        crate::stdlib::rebind_execution_state_builtins(vm);
    }

    fn set_source_dir(&self, dir: &Path) {
        crate::stdlib::set_thread_source_dir(dir);
    }

    fn enter_source_dir(&self, dir: Option<&Path>) -> Option<std::path::PathBuf> {
        crate::stdlib::process::enter_frame_source_dir(dir)
    }

    fn runtime_context_value(&self, vm: &Vm) -> VmValue {
        crate::runtime_context::runtime_context_value(vm)
    }

    fn is_replay(&self) -> bool {
        crate::triggers::dispatcher::is_replay()
    }

    fn warn(&self, category: &str, message: &str) {
        crate::events::log_warn(category, message);
    }

    fn is_step_function(&self, name: &str) -> bool {
        crate::step_runtime::is_step_function(name)
    }

    fn take_active_step(&self, frame_depth: usize) -> Option<crate::vm::step_context::ActiveStep> {
        crate::step_runtime::take_active_step(frame_depth)
    }

    fn before_step<'a>(
        &self,
        vm: &'a mut Vm,
        closure: &'a crate::value::VmClosure,
        args: Vec<VmValue>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VmValue>, VmError>> + Send + 'a>> {
        Box::pin(step_hooks::before(vm, closure, args))
    }

    fn finish_step<'a>(
        &self,
        vm: &'a mut Vm,
        step: crate::vm::step_context::ActiveStep,
        output: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(step_hooks::finish(vm, step, output))
    }

    fn is_tracked_function(&self, name: &str) -> bool {
        crate::step_runtime::is_tracked_function(name)
    }

    fn enter_function(&self, name: &str, frame_depth: usize, args: &[VmValue]) {
        crate::step_runtime::maybe_push_active_persona(name, frame_depth);
        crate::step_runtime::maybe_push_active_step(name, frame_depth, args);
    }

    fn prune_frames(&self, frame_depth: usize) {
        crate::step_runtime::prune_below_frame(frame_depth);
    }

    fn suspend_active_context(&self) -> crate::vm::step_context::ActiveContextGuard {
        crate::step_runtime::suspend_active_context()
    }

    fn step_error_boundary(
        &self,
        error: VmError,
        frame_depth: usize,
    ) -> crate::vm::step_context::StepBoundaryDecision {
        step_lifecycle::error_boundary(error, frame_depth)
    }

    fn finish_skipped_step(&self, frame_depth: usize, message: String) {
        crate::step_runtime::pop_and_record(frame_depth, "skipped", Some(message));
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

    fn serve_builtin_call(
        &self,
        name: &str,
        effects: Option<&'static [harn_builtin_meta::EffectSpec]>,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        if !crate::run_tape::prefix_active() {
            return None;
        }
        // No manifest contract at all (a host bridge builtin, say) is not the
        // same as a pure one: its effects are unknown, so the prefix refuses it.
        let Some(contract) = crate::stdlib::builtin_policy_metadata(name).1 else {
            return crate::run_tape::serve_read(None, name, None, args);
        };
        let owner = match contract.exposure {
            harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } => {
                Some((capability, method))
            }
            _ => None,
        };
        let effects = Some(effects.unwrap_or(contract.effects));
        crate::run_tape::serve_read(owner, name, effects, args)
    }

    fn records_builtin_calls(&self) -> bool {
        crate::run_tape::records_reads()
    }

    fn record_builtin_call(
        &self,
        name: &str,
        effects: Option<&'static [harn_builtin_meta::EffectSpec]>,
        args: &[VmValue],
        result: &Result<VmValue, VmError>,
    ) {
        crate::run_tape::record_read(None, name, effects, args, result);
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

struct NativeExecutionScope {
    ambient: crate::orchestration::AmbientExecutionScope,
    pools: Arc<crate::stdlib::pool::PoolRegistry>,
}

impl crate::vm::VmExecutionScope for NativeExecutionScope {
    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        self.ambient.poll_scoped(false, || {
            #[cfg(windows)]
            let _locks = crate::conditional_replace::scope_native_lock_root();
            crate::stdlib::pool::with_pool_registry_poll(&self.pools, || continuation(cx))
        })
    }
}

impl NativeExecutionState {
    fn capture_top_level_ambient(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> crate::orchestration::AmbientExecutionScope {
        crate::orchestration::AmbientExecutionScope::capture_for_top_level_execution(
            execution_id.clone(),
            self.llm_mock_context.clone(),
            self.worker_registry.clone(),
            self.daemon_registry.clone(),
            self.trigger_registry.clone(),
            self.session_runtime.clone(),
            self.tracing_runtime.clone(),
            self.agent_host_session_runtime.clone(),
        )
    }
}

/// Native execution services for the backend-independent interpreter.
pub trait NativeVmExt: Sized {
    fn set_bridge(&mut self, bridge: Arc<crate::bridge::HostBridge>);
    fn set_connector_clients(&mut self, clients: crate::connectors::VmConnectorClients);
    fn evaluate_decision(
        &mut self,
        site_id: &str,
        state: serde_json::Value,
        questions: serde_json::Value,
        policy: serde_json::Value,
    ) -> impl Future<Output = Result<crate::llm::decision::EvaluationResult, VmError>> + Send;
    fn set_evaluation_replay(&mut self, scope: crate::llm::decision::replay::EvaluationReplayScope);
    fn execution_evidence(
        &self,
        flight_recording: Option<crate::flight_recorder::FlightRecordingArtifact>,
        gaps: Vec<crate::orchestration::RunEvidenceGapRecord>,
    ) -> crate::orchestration::ExecutionEvidenceRecord;
}

/// Construct an interpreter with Harn's native execution services.
pub fn new_vm() -> Vm {
    crate::initialize_runtime_assets();
    Vm::with_runtime(Arc::new(NativeExecutionState::fresh()))
}

impl NativeVmExt for Vm {
    /// Set the bridge for delegating unknown builtins in bridge mode.
    fn set_bridge(&mut self, bridge: Arc<crate::bridge::HostBridge>) {
        self.native_mut().bridge = Some(bridge);
    }

    /// Install the connector projection owned by this VM execution tree.
    fn set_connector_clients(&mut self, clients: crate::connectors::VmConnectorClients) {
        self.native_mut().connector_clients = Arc::new(clients);
    }

    /// Evaluate a decision under this interpreter's execution and receipt owner.
    async fn evaluate_decision(
        &mut self,
        site_id: &str,
        state: serde_json::Value,
        questions: serde_json::Value,
        policy: serde_json::Value,
    ) -> Result<crate::llm::decision::EvaluationResult, VmError> {
        let ctx = self.prepare_host_call()?;
        let ambient = self.native().capture_top_level_ambient(self.execution_id());
        crate::orchestration::scope_ambient(ambient, async {
            crate::llm::decision::evaluate_json(&ctx, site_id, state, questions, policy).await
        })
        .await
    }
    /// Install an explicit decision recording/replay scope. Child VMs inherit
    /// it, including work scheduled on other threads; misses cannot go live.
    fn set_evaluation_replay(
        &mut self,
        scope: crate::llm::decision::replay::EvaluationReplayScope,
    ) {
        self.native_mut().evaluation.replay = Some(scope);
    }

    /// Snapshot the canonical evidence envelope for this execution tree.
    /// Hosts provide persistence outcomes; Harn owns identity, schema, spans,
    /// and the relationship between those facts.
    fn execution_evidence(
        &self,
        flight_recording: Option<crate::flight_recorder::FlightRecordingArtifact>,
        gaps: Vec<crate::orchestration::RunEvidenceGapRecord>,
    ) -> crate::orchestration::ExecutionEvidenceRecord {
        let mut gaps = gaps;
        let (evaluation_receipts, evaluation_dropped) =
            self.native().evaluation.journal.lock().snapshot();
        if evaluation_dropped > 0 {
            gaps.push(crate::orchestration::RunEvidenceGapRecord {
                component: "evaluation_receipts".into(),
                code: "receipt_limit_exceeded".into(),
                message: format!("{evaluation_dropped} evaluation receipts beyond the first 1024 were not retained"),
            });
        }
        let dropped = crate::llm::dropped_reasoning_receipts();
        if let Some(gap) = crate::llm::reasoning_receipt::overflow_gap(dropped) {
            // A truncated list must not read as the whole run. The overflow is
            // named as a gap rather than left to look like a short run.
            gaps.push(gap);
        }
        crate::orchestration::ExecutionEvidenceRecord {
            evaluation_receipts: Some(evaluation_receipts),
            reasoning_receipts: Some(crate::llm::peek_reasoning_receipts()),
            schema_version: crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION,
            execution_id: Some(self.execution_id().to_string()),
            trace_spans: self
                .native()
                .tracing_runtime
                .completed_spans_for_execution(self.execution_id())
                .iter()
                .map(crate::orchestration::RunTraceSpanRecord::from)
                .collect(),
            flight_recording,
            gaps,
        }
    }
}

impl VmRuntimeBaseline for NativeExecutionBaseline {
    fn instantiate(&self, source_dir: Option<&Path>) -> Arc<dyn VmRuntime> {
        crate::initialize_runtime_assets();
        if let Some(dir) = source_dir {
            crate::stdlib::set_thread_source_dir(dir);
        }
        Arc::new(NativeExecutionState::fresh())
    }
}

pub trait NativeVmState {
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
