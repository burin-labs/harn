use super::Vm;
use std::sync::Arc;

pub(crate) struct SpawnedTask {
    pub(crate) task: crate::value::VmTaskHandle,
    pub(crate) runtimes: Arc<crate::agent_lifecycle_cleanup::CleanupRuntimes>,
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
    pub(crate) runtimes: Arc<crate::agent_lifecycle_cleanup::CleanupRuntimes>,
}

impl Vm {
    /// Stop every outstanding child and transfer durable agent cleanup to the
    /// process-owned recovery runtime before this execution releases state.
    pub(crate) fn cancel_spawned_tasks(&mut self) {
        let runtimes = self.agent_cleanup_runtimes();
        for (_, task) in std::mem::take(&mut self.spawned_tasks) {
            super::ops::abort_task_detached(task);
        }
        for (_, pending) in std::mem::take(&mut self.pending_task_cleanups) {
            super::ops::call_support::schedule_task_cleanup(
                pending.task_id,
                pending.runtimes.as_ref().clone(),
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
        self.retained_agent_cleanup_runtimes().as_ref().clone()
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
                runtimes: self.retained_agent_cleanup_runtimes(),
            },
        );
    }

    pub(super) fn capture_agent_cleanup_runtimes(&mut self) {
        // Entry reads the VM's current runtimes, never a prior execution's
        // retained snapshot. Drop and scheduling must use this captured owner.
        self.cleanup_runtimes = Some(Arc::new(
            self.current_agent_cleanup_runtimes().capture_transport(),
        ));
    }

    fn retained_agent_cleanup_runtimes(
        &self,
    ) -> Arc<crate::agent_lifecycle_cleanup::CleanupRuntimes> {
        match &self.cleanup_runtimes {
            Some(runtimes) => runtimes.clone(),
            // Never-executed VMs have no originating transport to inherit.
            None => Arc::new(self.current_agent_cleanup_runtimes()),
        }
    }

    fn current_agent_cleanup_runtimes(&self) -> crate::agent_lifecycle_cleanup::CleanupRuntimes {
        crate::agent_lifecycle_cleanup::CleanupRuntimes::new(
            self.execution_id.to_string(),
            self.session_runtime.clone(),
            self.agent_host_session_runtime.clone(),
        )
    }
}
