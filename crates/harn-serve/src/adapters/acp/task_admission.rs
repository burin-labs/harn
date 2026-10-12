//! Trusted task admission at the session input and prompt boundaries.

use super::*;
use harn_session_store::{ControlEvent, ControlGoal};
use serde::{Deserialize, Serialize};

pub const TASK_ADMISSION_STORAGE_UNAVAILABLE_RPC_CODE: i64 = -32020;

/// Accepted API input accompanying a task-specific injection.
/// This is data; the host's configured admission owner decides whether it can act.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskInjectionTarget {
    pub task_id: String,
    pub message: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPromptTarget {
    pub task_id: String,
}

impl TaskPromptTarget {
    pub(crate) fn from_params(
        params: &serde_json::Value,
    ) -> Result<Option<Self>, TaskAdmissionError> {
        params
            .pointer("/_meta/harn/task")
            .map(|value| {
                let target: Self = serde_json::from_value(value.clone())
                    .map_err(|_| TaskAdmissionError::InvalidTaskContext)?;
                if target.task_id.trim().is_empty() {
                    return Err(TaskAdmissionError::InvalidTaskContext);
                }
                Ok(target)
            })
            .transpose()
    }

    pub(crate) fn project_into(
        &self,
        params: &mut serde_json::Value,
    ) -> Result<(), TaskAdmissionError> {
        if Self::from_params(params)?.is_some_and(|supplied| supplied != *self) {
            return Err(TaskAdmissionError::InvalidTaskContext);
        }
        let harn = params
            .as_object_mut()
            .ok_or(TaskAdmissionError::InvalidTaskContext)?
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or(TaskAdmissionError::InvalidTaskContext)?
            .entry("harn")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or(TaskAdmissionError::InvalidTaskContext)?;
        harn.insert(
            "task".into(),
            serde_json::to_value(self).map_err(|_| TaskAdmissionError::InvalidTaskContext)?,
        );
        Ok(())
    }
}

/// Host policy owns admission; client metadata only identifies the target task.
pub trait SessionTaskAdmission: Send + Sync {
    fn admit_input(
        &self,
        session_id: &str,
        control: &ControlEvent,
        task: Option<&TaskInjectionTarget>,
    ) -> Result<serde_json::Value, TaskAdmissionError>;

    fn admit_prompt(
        &self,
        session_id: &str,
        task: &TaskPromptTarget,
    ) -> Result<(), TaskAdmissionError>;
}

#[derive(Clone, Copy, Debug)]
pub enum TaskAdmissionError {
    InvalidTaskContext,
    TaskNotSteerable,
    TaskNotRunnable,
    TaskNotCurrent,
    StorageUnavailable,
}

impl TaskAdmissionError {
    pub fn rpc_code(self) -> i64 {
        match self {
            Self::InvalidTaskContext => -32602,
            Self::TaskNotSteerable | Self::TaskNotRunnable | Self::TaskNotCurrent => -32000,
            Self::StorageUnavailable => TASK_ADMISSION_STORAGE_UNAVAILABLE_RPC_CODE,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidTaskContext => "task context is invalid",
            Self::TaskNotSteerable => "task cannot accept this session input",
            Self::TaskNotRunnable => "task cannot dispatch this session prompt",
            Self::TaskNotCurrent => "task does not own the current session prompt",
            Self::StorageUnavailable => "durable task storage is unavailable",
        }
    }
}

pub(super) struct InjectionCommand {
    pub content: String,
    pub transcript_content: serde_json::Value,
    pub mode: String,
    pub requested_mode: String,
    pub goal: Option<ControlGoal>,
    pub actor: serde_json::Value,
}

impl ConcurrentSessionControls {
    pub(super) async fn enqueue_injection(
        &self,
        session_id: &str,
        inject_state: &harn_vm::bridge::HostBridgeInjectionState,
        params: &serde_json::Value,
        command: InjectionCommand,
    ) -> Result<serde_json::Value, TaskAdmissionError> {
        let target = params
            .pointer("/_meta/harn/taskMessage")
            .map(|value| serde_json::from_value::<TaskInjectionTarget>(value.clone()))
            .transpose()
            .map_err(|_| TaskAdmissionError::InvalidTaskContext)?;
        let mut projection = None;
        let message_id = inject_state
            .push_pending_user_message_admitted(
                command.content.clone(),
                command.transcript_content,
                &command.mode,
                |message_id| {
                    let control = ControlEvent::injection(
                        "session/inject",
                        control_id(),
                        "accepted",
                        command.requested_mode,
                        command.mode.clone(),
                        message_id.to_owned(),
                        command.content,
                    )
                    .with_goal(command.goal)
                    .with_actor(command.actor.clone());
                    let mut delivery_owner = harn_vm::bridge::UserMessageDeliveryOwner::Session;
                    if let Some(admission) = &self.task_admission {
                        projection = Some(match target.as_ref() {
                            Some(task) => self
                                .registered_control(session_id)
                                .ok_or(TaskAdmissionError::TaskNotSteerable)?
                                .with_current_task(&task.task_id, |_| {
                                    admission.admit_input(session_id, &control, Some(task))
                                })?,
                            None => admission.admit_input(session_id, &control, None)?,
                        });
                        if let Some(task) = &target {
                            delivery_owner = harn_vm::bridge::UserMessageDeliveryOwner::Task(
                                task.task_id.clone(),
                            );
                        }
                    }
                    self.record_inject_owner(
                        session_id,
                        message_id.to_owned(),
                        command.actor.clone(),
                    );
                    super::core::record_and_emit_control(self, session_id, control);
                    Ok(delivery_owner)
                },
            )
            .await?;
        let mut result = serde_json::json!({
            "messageId": message_id, "status": "accepted",
            "_meta": {"harn": {"actor": command.actor}},
        });
        if let Some(projection) = projection {
            result["_meta"]["harn"]["taskMessage"] = projection;
        }
        Ok(result)
    }
}
