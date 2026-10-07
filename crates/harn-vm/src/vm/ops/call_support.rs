use super::super::{PendingTaskCleanup, SpawnedTask};
use crate::value::{VmJoinHandle, VmValue};

pub(super) type VmTaskJoinResult =
    Result<Result<(VmValue, String), crate::value::VmError>, tokio::task::JoinError>;

/// Preserve the durable lifecycle owner whenever a joined task did not
/// complete successfully. Every join path delegates this decision here so a
/// newly added await/cancel surface cannot silently strand the task's journal.
pub(super) fn finish_task_join(
    joined: VmTaskJoinResult,
    cleanup: PendingTaskCleanup,
) -> VmTaskJoinResult {
    if !matches!(&joined, Ok(Ok(_))) {
        schedule_task_cleanup(cleanup.task_id, cleanup.runtimes.as_ref().clone());
    }
    joined
}

/// Stop a task from a synchronous unwind boundary, then finish its durable
/// agent lifecycle on an inherited runtime child. The journal and writer lease
/// remain visible if cleanup fails, so absence never masquerades as success.
pub(crate) fn abort_task_detached(owned: SpawnedTask) {
    let SpawnedTask { task, runtimes } = owned;
    let task_id = task.wait_task_id.clone();
    // A Tokio JoinHandle remains a value after yielding Ready, but polling it
    // again panics. Cleanup needs the task identity, not its discarded output,
    // so never hand an already-finished handle to the waiter.
    if task.handle.is_finished() {
        schedule_task_cleanup(task_id, runtimes.as_ref().clone());
        return;
    }
    task.cancel_token
        .store(true, std::sync::atomic::Ordering::SeqCst);
    task.handle.abort();
    schedule_task_cleanup_after(task_id, runtimes.as_ref().clone(), async move {
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

pub(super) async fn abort_task_and_wait(owned: SpawnedTask) -> Result<(), crate::value::VmError> {
    AwaitingTask::new(owned).cancel().await
}

pub(super) async fn abort_join_and_wait(handle: &mut VmJoinHandle) {
    handle.abort();
    let _ = handle.await;
}

pub(super) struct AwaitingTask {
    task: Option<SpawnedTask>,
}

impl AwaitingTask {
    pub(super) fn new(task: SpawnedTask) -> Self {
        Self { task: Some(task) }
    }

    /// Await the child while retaining cancellation ownership until its join
    /// handle resolves. A failed join still needs lifecycle cleanup, but the
    /// completed handle must never be polled a second time by the detached
    /// cleanup path.
    pub(super) async fn join(mut self) -> VmTaskJoinResult {
        let joined = self.wait().await;
        self.finish_join(joined)
    }

    pub(super) fn request_cancel(&self) {
        self.task
            .as_ref()
            .expect("awaiting task present")
            .task
            .cancel_token
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) async fn wait(&mut self) -> VmTaskJoinResult {
        (&mut self
            .task
            .as_mut()
            .expect("awaiting task present")
            .task
            .handle)
            .await
    }

    pub(super) fn finish_join(mut self, joined: VmTaskJoinResult) -> VmTaskJoinResult {
        let task = self.task.take().expect("awaiting task present after join");
        finish_task_join(joined, task.pending_cleanup())
    }

    pub(super) async fn cancel(mut self) -> Result<(), crate::value::VmError> {
        self.request_cancel();
        let task = self.task.as_mut().expect("awaiting task present");
        abort_join_and_wait(&mut task.task.handle).await;
        let result = task.runtimes.abandon_task(&task.task.wait_task_id).await;
        // The caller transfers any failed terminal write into its pending
        // registry synchronously. Until this point Drop still owns recovery.
        self.task.take();
        result
    }
}

impl Drop for AwaitingTask {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            abort_task_detached(task);
        }
    }
}

#[cfg(test)]
#[path = "task_cleanup_tests.rs"]
mod tests;

pub(super) enum StepPreHookAction {
    Allow(Vec<VmValue>),
    Deny(String),
}
