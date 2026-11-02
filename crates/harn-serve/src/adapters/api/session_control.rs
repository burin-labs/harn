//! Restore ACP continuity and its API projection under one configuration owner.

use super::*;
use harn_session_store::task::{TaskSessionDescriptor, TaskSessionUpdate};
use harn_session_store::{SessionStatus, StoreError, StoreResult};

pub(super) fn environment_policy(state: &ApiState) -> Value {
    json!(&state.session_environment)
}

#[derive(Debug)]
pub(super) enum SessionControlError {
    Store(StoreError),
    Restore(&'static str),
    Closed,
}

impl SessionControlError {
    pub(super) fn response(self) -> Response {
        match self {
            Self::Store(StoreError::NotFound(_)) => {
                api_error(StatusCode::NOT_FOUND, "not_found", "session not found")
            }
            Self::Store(_) => task_store::unavailable_response(),
            Self::Restore(operation) => api_error(
                StatusCode::BAD_GATEWAY,
                "acp_error",
                &format!("session could not be restored at {operation}"),
            ),
            Self::Closed => api_error(StatusCode::CONFLICT, "session_closed", "session is closed"),
        }
    }
}

impl From<StoreError> for SessionControlError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl ApiState {
    /// The caller retains this guard through control acknowledgements and
    /// descriptor publication, then releases it before any prompt executes.
    pub(super) async fn restore_session(
        &self,
        session_id: &str,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>, SessionControlError> {
        let guard = self.session_control.lock().await;
        if self
            .inner
            .lock()
            .expect("api state poisoned")
            .sessions
            .contains_key(session_id)
        {
            return Ok(guard);
        }
        let resource = self.session_resource(session_id)?;
        let descriptor = self.session_descriptor(session_id)?;
        if descriptor.status == SessionStatus::Open {
            self.acp
                .call(
                    "session/load",
                json!({"sessionId":session_id, "cwd":descriptor.workspace_root, "environmentPolicy":environment_policy(self)}),
                )
                .await
            .map_err(|_| SessionControlError::Restore("session/load"))?;
            self.acp
                .call(
                    "session/set_mode",
                    json!({"sessionId":session_id, "modeId":descriptor.mode_id}),
                )
                .await
                .map_err(|_| SessionControlError::Restore("session/set_mode"))?;
            sessions::restore_model_policy(self, session_id, descriptor.model_policy.as_ref())
                .await
                .map_err(|_| SessionControlError::Restore("model policy"))?;
        }
        let mut inner = self.inner.lock().expect("api state poisoned");
        inner.sessions.insert(session_id.into(), resource);
        inner.messages.entry(session_id.into()).or_default();
        Ok(guard)
    }

    pub(super) fn require_open_session(&self, session_id: &str) -> Result<(), SessionControlError> {
        let resource = self.session_resource(session_id)?;
        if resource["state"] == "CLOSED" {
            return Err(SessionControlError::Closed);
        }
        Ok(())
    }

    pub(super) fn update_session_facts(
        &self,
        session_id: &str,
        update: TaskSessionUpdate,
    ) -> StoreResult<Option<TaskSessionDescriptor>> {
        let store = self.task_store()?;
        match store.get_task_session(session_id) {
            Ok(_) => {
                let change = store.update_task_session(session_id, update)?;
                for event in change.events {
                    self.publish_task_event(event);
                }
                store.get_task_session(session_id).map(Some)
            }
            Err(StoreError::NotFound(_)) => Ok(None), // Standalone API sessions remain process-local.
            Err(error) => Err(error),
        }
    }
}
