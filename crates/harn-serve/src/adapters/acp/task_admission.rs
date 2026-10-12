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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPromptTarget {
    pub task_id: String,
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
    StorageUnavailable,
}

impl TaskAdmissionError {
    pub fn rpc_code(self) -> i64 {
        match self {
            Self::InvalidTaskContext => -32602,
            Self::TaskNotSteerable | Self::TaskNotRunnable => -32000,
            Self::StorageUnavailable => TASK_ADMISSION_STORAGE_UNAVAILABLE_RPC_CODE,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidTaskContext => "task context is invalid",
            Self::TaskNotSteerable => "task cannot accept this session input",
            Self::TaskNotRunnable => "task cannot dispatch this session prompt",
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
                    if let Some(admission) = &self.task_admission {
                        projection =
                            Some(admission.admit_input(session_id, &control, target.as_ref())?);
                    }
                    self.record_inject_owner(
                        session_id,
                        message_id.to_owned(),
                        command.actor.clone(),
                    );
                    super::core::record_and_emit_control(self, session_id, control);
                    Ok(())
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
