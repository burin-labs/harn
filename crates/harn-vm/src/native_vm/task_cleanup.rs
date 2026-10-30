//! Native lifecycle retention and recovery behind the interpreter contract.

use super::NativeExecutionState;
#[cfg(test)]
use super::NativeVmState;
#[cfg(test)]
use crate::vm::Vm;
use crate::{agent_lifecycle_cleanup::CleanupRuntimes, vm::VmTaskCleanup};
use std::{future::Future, pin::Pin, sync::Arc};

impl NativeExecutionState {
    pub(super) fn current_cleanup_runtimes(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> CleanupRuntimes {
        CleanupRuntimes::new(
            execution_id.to_string(),
            self.session_runtime.clone(),
            self.agent_host_session_runtime.clone(),
        )
    }

    pub(super) fn retained_cleanup_runtimes(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> Arc<CleanupRuntimes> {
        match &self.cleanup_runtimes {
            Some(runtimes) => runtimes.clone(),
            // Never-executed VMs have no originating transport to inherit.
            None => Arc::new(self.current_cleanup_runtimes(execution_id)),
        }
    }
}

#[cfg(test)]
pub(crate) trait NativeCleanupTestExt {
    fn agent_cleanup_runtimes(&self) -> CleanupRuntimes;
}

#[cfg(test)]
impl NativeCleanupTestExt for Vm {
    fn agent_cleanup_runtimes(&self) -> CleanupRuntimes {
        self.native()
            .retained_cleanup_runtimes(self.execution_id())
            .as_ref()
            .clone()
    }
}

impl VmTaskCleanup for CleanupRuntimes {
    fn abandon_task<'a>(
        &'a self,
        task_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::value::VmError>> + Send + 'a>> {
        CleanupRuntimes::abandon_task(self, task_id)
    }

    fn schedule(&self, task_id: String, before_cleanup: Pin<Box<dyn Future<Output = ()> + Send>>) {
        let runtimes = self.clone();
        crate::task_spawn::spawn_lifecycle_cleanup(async move {
            before_cleanup.await;
            crate::agent_lifecycle_cleanup::schedule(task_id, runtimes);
        });
    }
}
