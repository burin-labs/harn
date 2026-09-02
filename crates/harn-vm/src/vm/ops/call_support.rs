use crate::value::{VmJoinHandle, VmTaskHandle, VmValue};

/// Stop a task from a synchronous unwind boundary, then finish its durable
/// agent lifecycle on an inherited runtime child. The journal and writer lease
/// remain visible if cleanup fails, so absence never masquerades as success.
pub(crate) fn abort_task_detached(
    registry: std::sync::Arc<crate::stdlib::pool::PoolRegistry>,
    task: VmTaskHandle,
) {
    let task_id = task.wait_task_id.clone();
    task.cancel_token
        .store(true, std::sync::atomic::Ordering::SeqCst);
    task.handle.abort();
    crate::vm::subtask::spawn_child(registry, async move {
        let _ = task.handle.await;
        if let Err(error) =
            crate::llm::agent_session_host::cancellation::abandon_task_sessions(&task_id).await
        {
            crate::events::log_warn(
                "agent.task_cancel_cleanup",
                &format!("task={task_id} durable cleanup remains pending: {error}"),
            );
        }
    });
}

pub(super) async fn abort_task_and_wait(
    mut task: VmTaskHandle,
) -> Result<(), crate::value::VmError> {
    let task_id = task.wait_task_id.clone();
    task.cancel_token
        .store(true, std::sync::atomic::Ordering::SeqCst);
    abort_join_and_wait(&mut task.handle).await;
    crate::llm::agent_session_host::cancellation::abandon_task_sessions(&task_id).await
}

pub(super) async fn abort_join_and_wait(handle: &mut VmJoinHandle) {
    handle.abort();
    let _ = handle.await;
}

pub(super) struct AwaitingTask {
    task: Option<VmTaskHandle>,
    registry: std::sync::Arc<crate::stdlib::pool::PoolRegistry>,
}

impl AwaitingTask {
    pub(super) fn new(
        task: VmTaskHandle,
        registry: std::sync::Arc<crate::stdlib::pool::PoolRegistry>,
    ) -> Self {
        Self {
            task: Some(task),
            registry,
        }
    }

    pub(super) fn handle_mut(&mut self) -> &mut VmJoinHandle {
        &mut self.task.as_mut().expect("awaiting task present").handle
    }

    pub(super) fn disarm(mut self) {
        self.task = None;
    }
}

impl Drop for AwaitingTask {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            abort_task_detached(self.registry.clone(), task);
        }
    }
}

pub(super) enum StepPreHookAction {
    Allow(Vec<VmValue>),
    Deny(String),
}
