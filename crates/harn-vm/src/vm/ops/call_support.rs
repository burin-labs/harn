use crate::value::{VmJoinHandle, VmTaskHandle, VmValue};

pub(super) async fn abort_task_and_wait(mut task: VmTaskHandle) {
    task.cancel_token
        .store(true, std::sync::atomic::Ordering::SeqCst);
    abort_join_and_wait(&mut task.handle).await;
}

pub(super) async fn abort_join_and_wait(handle: &mut VmJoinHandle) {
    handle.abort();
    let _ = handle.await;
}

pub(super) struct AwaitingTask {
    task: Option<VmTaskHandle>,
}

impl AwaitingTask {
    pub(super) fn new(task: VmTaskHandle) -> Self {
        Self { task: Some(task) }
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
            task.cancel_token
                .store(true, std::sync::atomic::Ordering::SeqCst);
            task.handle.abort();
        }
    }
}

pub(super) enum StepPreHookAction {
    Allow(Vec<VmValue>),
    Deny(String),
}
