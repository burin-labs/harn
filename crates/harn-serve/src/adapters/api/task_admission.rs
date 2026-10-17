//! Project ACP task admission into durable task lifecycle and control history.

use super::*;
use crate::adapters::acp::{
    SessionTaskAdmission, TaskAdmissionError, TaskInjectionTarget, TaskPromptTarget,
};
use crate::sessions::SharedSessionStore;
use harn_session_store::task::{TaskStatus, TaskUpdate};
use harn_session_store::{ControlEvent, StoreError};

pub(super) struct NativeTaskAdmission {
    pub store: Option<SharedSessionStore>,
    pub inner: Arc<Mutex<ApiStateInner>>,
    pub events_tx: broadcast::Sender<ApiEvent>,
}

impl SessionTaskAdmission for NativeTaskAdmission {
    fn admit_input(
        &self,
        session_id: &str,
        control: &ControlEvent,
        task: Option<&TaskInjectionTarget>,
    ) -> Result<Value, TaskAdmissionError> {
        let target = task.ok_or(TaskAdmissionError::InvalidTaskContext)?;
        let message_id = control
            .message_id
            .as_ref()
            .ok_or(TaskAdmissionError::InvalidTaskContext)?;
        if prompt_text(&target.message).as_ref() != control.text.as_ref() {
            return Err(TaskAdmissionError::InvalidTaskContext);
        }
        let store = self
            .store
            .as_ref()
            .and_then(|store| store.task_store())
            .ok_or(TaskAdmissionError::StorageUnavailable)?;
        let message = message_resource_with_id(
            session_id,
            Some(&target.task_id),
            target.message.clone(),
            message_id.clone(),
        );
        let event = store
            .append_task_input(&target.task_id, session_id, message)
            .map_err(|error| match error {
                StoreError::NotFound(_) | StoreError::InvalidInput(_) => {
                    TaskAdmissionError::TaskNotSteerable
                }
                _ => TaskAdmissionError::StorageUnavailable,
            })?;
        let message = event.payload.clone();
        task_store::publish_task_event(&self.inner, &self.events_tx, event);
        Ok(message)
    }

    fn admit_prompt(
        &self,
        session_id: &str,
        target: &TaskPromptTarget,
    ) -> Result<(), TaskAdmissionError> {
        let store = self
            .store
            .as_ref()
            .and_then(|store| store.task_store())
            .ok_or(TaskAdmissionError::StorageUnavailable)?;
        let task = store
            .get_task(&target.task_id)
            .map_err(|error| match error {
                StoreError::NotFound(_) => TaskAdmissionError::TaskNotRunnable,
                _ => TaskAdmissionError::StorageUnavailable,
            })?;
        if task.session_id != session_id || task.status != TaskStatus::Submitted {
            return Err(TaskAdmissionError::TaskNotRunnable);
        }
        let change = store
            .update_task(
                &target.task_id,
                TaskUpdate {
                    status: TaskStatus::Working,
                    at: now_rfc3339(),
                    failure: None,
                },
            )
            .map_err(|_| TaskAdmissionError::StorageUnavailable)?;
        if change.task.status != TaskStatus::Working
            || change.previous_status != TaskStatus::Submitted
        {
            return Err(TaskAdmissionError::TaskNotRunnable);
        }
        if let Some(event) = change.event {
            task_store::publish_task_event(&self.inner, &self.events_tx, event);
        }
        Ok(())
    }
}
