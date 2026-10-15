use super::Vm;
use std::sync::Arc;

pub(crate) struct SpawnedTask {
    pub(crate) task: crate::value::VmTaskHandle,
    pub(crate) runtimes: Arc<dyn super::VmTaskCleanup>,
}

impl SpawnedTask {
    pub(crate) fn pending_cleanup(&self) -> PendingTaskCleanup {
        PendingTaskCleanup {
            task_id: self.task.wait_task_id.clone(),
            runtimes: self.runtimes.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct PendingTaskCleanup {
    pub(crate) task_id: String,
    pub(crate) runtimes: Arc<dyn super::VmTaskCleanup>,
}

impl Vm {
    /// Stop every outstanding child and transfer durable agent cleanup to the
    /// process-owned recovery runtime before this execution releases state.
    pub(crate) fn cancel_spawned_tasks(&mut self) {
        let runtimes = self.retained_task_cleanup();
        for (_, task) in std::mem::take(&mut self.spawned_tasks) {
            super::ops::abort_task_detached(task);
        }
        for (_, pending) in std::mem::take(&mut self.pending_task_cleanups) {
            super::ops::call_support::schedule_task_cleanup(pending.task_id, pending.runtimes);
        }
        // A top-level VM can own an agent lifecycle independently of spawned
        // children. Inline child VMs share that identity but must not activate
        // their parent's reservation when their temporary call context drops.
        if self.owns_execution {
            super::ops::call_support::schedule_task_cleanup(
                self.runtime_context.task_id.clone(),
                runtimes,
            );
        }
    }

    pub(crate) fn register_spawned_task(
        &mut self,
        public_id: String,
        task: crate::value::VmTaskHandle,
    ) {
        self.spawned_tasks.insert(
            public_id,
            SpawnedTask {
                task,
                runtimes: self.retained_task_cleanup(),
            },
        );
    }

    fn retained_task_cleanup(&self) -> Arc<dyn super::VmTaskCleanup> {
        self.runtime.task_cleanup(&self.execution_id)
    }
}
