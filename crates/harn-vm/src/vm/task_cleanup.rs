use super::Vm;

#[derive(Clone)]
pub(crate) struct PendingTaskCleanup {
    pub(crate) execution_id: String,
    pub(crate) task_id: String,
}

impl Vm {
    /// Stop every outstanding child and transfer durable agent cleanup to the
    /// process-owned recovery runtime before this execution releases state.
    pub(crate) fn cancel_spawned_tasks(&mut self) {
        let runtimes = self.agent_cleanup_runtimes();
        for (_, task) in std::mem::take(&mut self.spawned_tasks) {
            super::ops::abort_task_detached(task, runtimes.clone());
        }
        for (_, pending) in std::mem::take(&mut self.pending_task_cleanups) {
            super::ops::call_support::schedule_task_cleanup(
                pending.task_id,
                self.agent_cleanup_runtimes_for_execution(pending.execution_id),
            );
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

    pub(crate) fn agent_cleanup_runtimes(&self) -> crate::agent_lifecycle_cleanup::CleanupRuntimes {
        self.agent_cleanup_runtimes_for_execution(self.execution_id.to_string())
    }

    fn agent_cleanup_runtimes_for_execution(
        &self,
        execution_id: String,
    ) -> crate::agent_lifecycle_cleanup::CleanupRuntimes {
        crate::agent_lifecycle_cleanup::CleanupRuntimes::new(
            execution_id,
            self.session_runtime.clone(),
            self.agent_host_session_runtime.clone(),
        )
    }
}
