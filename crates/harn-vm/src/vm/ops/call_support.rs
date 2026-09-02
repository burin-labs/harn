use crate::value::{VmJoinHandle, VmTaskHandle, VmValue};

/// Stop a task from a synchronous unwind boundary, then finish its durable
/// agent lifecycle on an inherited runtime child. The journal and writer lease
/// remain visible if cleanup fails, so absence never masquerades as success.
pub(crate) fn abort_task_detached(
    task: VmTaskHandle,
    runtimes: crate::agent_lifecycle_cleanup::CleanupRuntimes,
) {
    let task_id = task.wait_task_id.clone();
    // A Tokio JoinHandle remains a value after yielding Ready, but polling it
    // again panics. Cleanup needs the task identity, not its discarded output,
    // so never hand an already-finished handle to the waiter.
    if task.handle.is_finished() {
        schedule_task_cleanup(task_id, runtimes);
        return;
    }
    task.cancel_token
        .store(true, std::sync::atomic::Ordering::SeqCst);
    task.handle.abort();
    schedule_task_cleanup_after(task_id, runtimes, async move {
        let _ = task.handle.await;
    });
}

/// Activate the process-owned cleanup reservation established before the agent
/// session started. Recovery retries until the terminal write commits; it has
/// no late admission or retry-exhaustion path that can discard the owner.
pub(crate) fn schedule_task_cleanup(
    task_id: String,
    runtimes: crate::agent_lifecycle_cleanup::CleanupRuntimes,
) {
    schedule_task_cleanup_after(task_id, runtimes, std::future::ready(()));
}

fn schedule_task_cleanup_after<F>(
    task_id: String,
    runtimes: crate::agent_lifecycle_cleanup::CleanupRuntimes,
    before_cleanup: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    crate::vm::subtask::spawn_lifecycle_cleanup(async move {
        before_cleanup.await;
        crate::agent_lifecycle_cleanup::schedule(task_id, runtimes);
    });
}

pub(super) async fn abort_task_and_wait(
    mut task: VmTaskHandle,
    execution_id: &str,
) -> Result<(), crate::value::VmError> {
    let task_id = task.wait_task_id.clone();
    task.cancel_token
        .store(true, std::sync::atomic::Ordering::SeqCst);
    abort_join_and_wait(&mut task.handle).await;
    crate::llm::agent_session_host::cancellation::abandon_task_sessions(execution_id, &task_id)
        .await
}

pub(super) async fn abort_join_and_wait(handle: &mut VmJoinHandle) {
    handle.abort();
    let _ = handle.await;
}

pub(super) struct AwaitingTask {
    task: Option<VmTaskHandle>,
    runtimes: crate::agent_lifecycle_cleanup::CleanupRuntimes,
}

impl AwaitingTask {
    pub(super) fn new(
        task: VmTaskHandle,
        runtimes: crate::agent_lifecycle_cleanup::CleanupRuntimes,
    ) -> Self {
        Self {
            task: Some(task),
            runtimes,
        }
    }

    /// Await the child while retaining cancellation ownership until its join
    /// handle resolves. A failed join still needs lifecycle cleanup, but the
    /// completed handle must never be polled a second time by the detached
    /// cleanup path.
    pub(super) async fn join(
        mut self,
    ) -> Result<Result<(VmValue, String), crate::value::VmError>, tokio::task::JoinError> {
        let joined = (&mut self.task.as_mut().expect("awaiting task present").handle).await;
        let task = self.task.take().expect("awaiting task present after join");
        if !matches!(&joined, Ok(Ok(_))) {
            schedule_task_cleanup(task.wait_task_id, self.runtimes.clone());
        }
        joined
    }
}

impl Drop for AwaitingTask {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            abort_task_detached(task, self.runtimes.clone());
        }
    }
}

pub(super) enum StepPreHookAction {
    Allow(Vec<VmValue>),
    Deny(String),
}
