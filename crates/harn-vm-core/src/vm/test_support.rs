use super::{TaskScope, Vm};
use crate::{VmError, VmValue};

pub fn reset_stdlib_cache_for_test() {
    super::stdlib_artifact::reset_stdlib_module_artifact_cache();
}

pub fn stdlib_cached_ptr_for_test(module: &str) -> Option<usize> {
    let source = harn_modules::stdlib_catalog()
        .source(module)
        .expect("stdlib module exists");
    super::stdlib_artifact::stdlib_module_artifact_cache_ptr(module, source)
}

pub fn module_interface_resolution_count_for_test() -> u64 {
    crate::module_artifact::INTERFACE_RESOLUTIONS.with(std::cell::Cell::get)
}

pub fn stdlib_warm_lock_file_for_test() -> &'static str {
    super::stdlib_artifact::STDLIB_WARM_LOCK_FILE
}

pub fn compile_stdlib_source_for_test(
    path: &std::path::Path,
    source: &str,
    context: &crate::module_artifact::ModuleCompilationContext,
) -> Result<crate::module_artifact::ModuleArtifact, VmError> {
    crate::module_artifact::compile_embedded_stdlib_module_artifact_from_source_with_context(
        path, source, context,
    )
}
impl Vm {
    pub fn register_spawned_task_for_test(&mut self, public_id: String, task: crate::VmTaskHandle) {
        self.register_spawned_task(public_id, task);
    }
    pub fn has_spawned_task_for_test(&self, public_id: &str) -> bool {
        self.spawned_tasks.contains_key(public_id)
    }
    pub fn has_pending_cleanup_for_test(&self, public_id: &str) -> bool {
        self.pending_task_cleanups.contains_key(public_id)
    }
    pub fn spawned_task_finished_for_test(&self, public_id: &str) -> bool {
        self.spawned_tasks[public_id].task.handle.is_finished()
    }
    pub fn retain_pending_cleanup_for_test(
        &mut self,
        public_id: String,
        task_id: String,
        runtimes: std::sync::Arc<dyn super::VmTaskCleanup>,
    ) {
        self.pending_task_cleanups
            .insert(public_id, super::PendingTaskCleanup { task_id, runtimes });
    }
    pub fn prepare_execution_for_test(&mut self) {
        self.prepare_execution_for_top_level();
    }
    pub fn builtin_handler_for_test(&self, name: &str) -> Option<&crate::VmBuiltinFn> {
        self.builtins.get(name)
    }
    pub fn shares_builtin_tables_for_test(&self, other: &Self) -> bool {
        use std::sync::Arc;
        Arc::ptr_eq(&self.builtins, &other.builtins)
            && Arc::ptr_eq(&self.async_builtins, &other.async_builtins)
            && Arc::ptr_eq(&self.capability_methods, &other.capability_methods)
            && Arc::ptr_eq(&self.builtin_metadata, &other.builtin_metadata)
            && Arc::ptr_eq(&self.builtins_by_id, &other.builtins_by_id)
            && Arc::ptr_eq(&self.builtin_id_collisions, &other.builtin_id_collisions)
    }
    pub fn deadline_probe_for_test(&self) -> VmDeadlineProbe {
        VmDeadlineProbe {
            state: self.execution_deadline.clone(),
        }
    }
    pub fn module_cached_for_test(&self, path: &std::path::Path) -> bool {
        self.module_cache.contains_key(path)
    }
    pub fn module_instantiation_count_for_test(&self, path: &std::path::Path) -> usize {
        self.shared_module_instances.instantiation_count(path)
    }
    pub fn error_stack_trace_for_test(&self) -> &Vec<(String, usize, usize, Option<String>)> {
        &self.error_stack_trace
    }
    pub fn register_interrupt_handler_for_test(
        &mut self,
        handler: VmValue,
        opts: Option<&VmValue>,
    ) -> Result<VmValue, VmError> {
        self.register_interrupt_handler(handler, opts)
    }
    pub async fn pending_scope_interrupt_for_test(&mut self) -> Option<VmError> {
        self.pending_scope_interrupt().await
    }
    pub fn has_unfiltered_interrupt_handler_for_test(&self) -> bool {
        self.has_unfiltered_interrupt_handler()
    }
    pub async fn call_builtin_id_for_test(
        &mut self,
        id: crate::BuiltinId,
        name: &str,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        self.call_builtin_id_or_name(id, name, args).await
    }
    pub fn try_call_sync_builtin_for_test(
        &mut self,
        id: Option<crate::BuiltinId>,
        name: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        self.try_call_sync_builtin_id_or_name_args(id, name, &super::CallArgs::Slice(args))
    }
    pub fn try_call_stack_builtin_for_test(
        &mut self,
        id: Option<crate::BuiltinId>,
        name: &str,
        argc: usize,
    ) -> Option<Result<VmValue, VmError>> {
        self.try_call_sync_builtin_id_or_name_from_stack_args(id, name, argc)
    }
}

/// A read-only view of a retained deadline during an active host call.
pub struct VmDeadlineProbe {
    state: std::sync::Arc<super::state::ExecutionDeadlineState>,
}
impl VmDeadlineProbe {
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }
    pub fn current(&self) -> Option<std::time::Instant> {
        self.state.current()
    }
}

pub fn abort_task_for_test(
    task: crate::VmTaskHandle,
    runtimes: std::sync::Arc<dyn super::VmTaskCleanup>,
) {
    super::ops::abort_task_detached(super::SpawnedTask { task, runtimes });
}

impl Vm {
    #[doc(hidden)]
    pub async fn join_task_scope_for_test(&mut self, task_ids: Vec<String>) -> Result<(), VmError> {
        self.task_scopes.push(TaskScope {
            task_ids,
            frame_depth: 0,
            env_scope_depth: 0,
        });
        self.execute_task_scope_exit().await
    }

    #[doc(hidden)]
    pub async fn call_special_for_test(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        assert!(self.try_call_special_name(name, args).await?);
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }
}
