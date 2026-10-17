//! Durable task admission, execution descriptors, and task control history.
//!
//! These facts live at the API's root store. Agent transcripts retain their
//! existing owner in each session's actual workspace.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{SessionStatus, StoreError, StoreHooks, StoreResult, TaskExecutionLease};

pub const TASK_RETRY_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskAdmission<T> {
    Created(T),
    Reused(T),
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    Submitted,
    Working,
    InputRequired,
    AuthRequired,
    Completed,
    Failed,
    Canceled,
}

impl TaskStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Submitted | Self::Working | Self::InputRequired | Self::AuthRequired
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "SUBMITTED",
            Self::Working => "WORKING",
            Self::InputRequired => "INPUT_REQUIRED",
            Self::AuthRequired => "AUTH_REQUIRED",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Canceled => "CANCELED",
        }
    }

    fn event(self) -> &'static str {
        match self {
            Self::Submitted => "task.submitted",
            Self::Working => "task.resumed",
            Self::InputRequired => "task.input_required",
            Self::AuthRequired => "task.auth_required",
            Self::Completed => "task.completed",
            Self::Failed => "task.failed",
            Self::Canceled => "task.canceled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFailure {
    pub code: String,
    pub message: String,
}

/// Execution configuration needed to recover a task's session identity.
/// This is a descriptor, not a second transcript or a cached HTTP response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSessionDescriptor {
    pub id: String,
    pub workspace_id: String,
    pub workspace_root: String,
    pub mode_id: String,
    pub model_policy: Option<Value>,
    pub metadata: Value,
    pub persona_id: Option<String>,
    pub root_session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub branch_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default = "open_session_status")]
    pub status: SessionStatus,
}

fn open_session_status() -> SessionStatus {
    SessionStatus::Open
}

/// Mutable session facts. Identity and transcript ownership cannot change here.
#[derive(Clone, Debug, Default)]
pub struct TaskSessionUpdate {
    pub metadata: Option<Value>,
    pub summary: Option<Option<String>>,
    pub model_policy: Option<Option<Value>>,
    pub status: Option<SessionStatus>,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct TaskSessionChange {
    pub session: TaskSessionDescriptor,
    /// Closing a session atomically cancels its accepted unfinished tasks.
    pub events: Vec<TaskControlEvent>,
}

impl TaskSessionUpdate {
    pub(crate) fn apply(self, session: &mut TaskSessionDescriptor) -> StoreResult<()> {
        if self.updated_at.is_empty()
            || self
                .metadata
                .as_ref()
                .is_some_and(|value| !value.is_object())
            || self
                .model_policy
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|value| !value.is_object())
            || (session.status != SessionStatus::Open && self.status == Some(SessionStatus::Open))
        {
            return Err(StoreError::InvalidInput(
                "invalid task session update".into(),
            ));
        }
        if let Some(metadata) = self.metadata {
            session.metadata = metadata;
        }
        if let Some(summary) = self.summary {
            session.summary = summary;
        }
        if let Some(policy) = self.model_policy {
            session.model_policy = policy;
        }
        if let Some(status) = self.status {
            session.status = status;
        }
        session.updated_at = self.updated_at;
        Ok(())
    }
}

impl TaskSessionDescriptor {
    pub(crate) fn redacted(mut self, hooks: &StoreHooks) -> Self {
        if let Some(redactor) = hooks.redaction.as_ref() {
            redactor.redact_json_in_place(&mut self.metadata);
            if let Some(policy) = self.model_policy.as_mut() {
                redactor.redact_json_in_place(policy);
            }
            if let Some(summary) = self.summary.take() {
                let mut summary = Value::String(summary);
                redactor.redact_json_in_place(&mut summary);
                self.summary = summary.as_str().map(str::to_owned);
            }
        }
        self
    }
}

/// Domain facts independent of HTTP status, headers, or response encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: TaskStatus,
    pub input: Value,
    pub metadata: Value,
    pub created_by: String,
    pub persona_id: Option<String>,
    pub branch_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub assigned_agent_id: Option<String>,
    pub receipt_id: Option<String>,
    pub outcome_id: Option<String>,
    pub quota_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub canceled_at: Option<String>,
    pub failure: Option<TaskFailure>,
}

impl TaskRecord {
    pub fn resource(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("typed task record serializes");
        value["object"] = Value::String("task".into());
        value
    }

    pub(crate) fn redacted(mut self, hooks: &StoreHooks) -> StoreResult<Self> {
        if let Some(redactor) = hooks.redaction.as_ref() {
            redactor.redact_json_in_place(&mut self.input);
            redactor.redact_json_in_place(&mut self.metadata);
            if let Some(failure) = self.failure.as_mut() {
                let mut message = Value::String(failure.message.clone());
                redactor.redact_json_in_place(&mut message);
                failure.message = message
                    .as_str()
                    .ok_or_else(|| {
                        StoreError::Backend("task redactor changed failure message shape".into())
                    })?
                    .to_owned();
            }
        }
        Ok(self)
    }

    pub(crate) fn transition(&mut self, update: TaskUpdate) -> bool {
        let allowed = match self.status {
            TaskStatus::Submitted => matches!(
                update.status,
                TaskStatus::Working | TaskStatus::Failed | TaskStatus::Canceled
            ),
            TaskStatus::Working => matches!(
                update.status,
                TaskStatus::InputRequired
                    | TaskStatus::AuthRequired
                    | TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Canceled
            ),
            TaskStatus::InputRequired | TaskStatus::AuthRequired => matches!(
                update.status,
                TaskStatus::Working | TaskStatus::Failed | TaskStatus::Canceled
            ),
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Canceled => false,
        };
        if !allowed {
            return false;
        }
        self.status = update.status;
        self.updated_at = update.at.clone();
        match update.status {
            TaskStatus::Completed => {
                self.completed_at = Some(update.at);
                self.outcome_id = Some(format!("outcome_{}", self.id));
            }
            TaskStatus::Canceled => self.canceled_at = Some(update.at),
            TaskStatus::Failed => self.failure = update.failure,
            TaskStatus::Working => {
                self.started_at.get_or_insert(update.at);
            }
            TaskStatus::Submitted | TaskStatus::InputRequired | TaskStatus::AuthRequired => {}
        }
        true
    }
}

#[derive(Clone, Debug)]
pub struct TaskUpdate {
    pub status: TaskStatus,
    pub at: String,
    pub failure: Option<TaskFailure>,
}

#[derive(Clone, Debug)]
pub struct TaskChange {
    pub task: TaskRecord,
    pub previous_status: TaskStatus,
    /// Present only when this call committed a lifecycle transition.
    pub event: Option<TaskControlEvent>,
}

/// Current message projection plus the exact audited prefix it observed.
#[derive(Clone, Debug)]
pub struct TaskMessageView {
    pub events: Vec<TaskControlEvent>,
    pub through_event_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct TaskMessageProjection {
    through_event_id: Option<String>,
    retained_event_ids: std::collections::BTreeSet<String>,
}

impl TaskMessageProjection {
    pub(crate) fn includes(&self, id: &str) -> bool {
        self.through_event_id
            .as_deref()
            .is_none_or(|through| id > through)
            || self.retained_event_ids.contains(id)
    }

    pub(crate) fn view(&self, events: Vec<TaskControlEvent>) -> TaskMessageView {
        let through_event_id = events.last().map(|event| event.id.clone());
        TaskMessageView {
            events: events
                .into_iter()
                .filter(|event| self.includes(&event.id))
                .collect(),
            through_event_id,
        }
    }

    pub(crate) fn retain(
        &mut self,
        events: &[TaskControlEvent],
        through: Option<&str>,
        retained: &[String],
    ) -> StoreResult<()> {
        let Some(through) = through else {
            return Ok(());
        };
        if !events.iter().any(|event| event.id == through) {
            return Err(StoreError::InvalidInput(
                "unknown task message snapshot".into(),
            ));
        }
        let watermark = self
            .through_event_id
            .as_deref()
            .map_or(through, |old| old.max(through));
        let retained: std::collections::BTreeSet<&str> =
            retained.iter().map(String::as_str).collect();
        let visible = events
            .iter()
            .filter(|event| {
                event.id.as_str() <= watermark
                    && self.includes(&event.id)
                    && (event.id.as_str() > through || retained.contains(event.id.as_str()))
            })
            .map(|event| event.id.clone())
            .collect();
        self.through_event_id = Some(watermark.to_owned());
        self.retained_event_ids = visible;
        Ok(())
    }
}

/// Validated scope and request digests; storage never receives a raw key.
#[derive(Clone, Debug)]
pub struct StoredTaskRetry {
    pub binding_digest: String,
    pub request_fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct NewTaskControlEvent {
    pub event: String,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskControlEvent {
    pub id: String,
    pub task_id: String,
    pub session_id: String,
    pub created_at: String,
    pub event: String,
    pub payload: Value,
}

impl TaskControlEvent {
    pub(crate) fn new(
        id: String,
        task: &TaskRecord,
        event: NewTaskControlEvent,
        hooks: &StoreHooks,
    ) -> Self {
        let mut record = Self {
            id,
            task_id: task.id.clone(),
            session_id: task.session_id.clone(),
            created_at: crate::event::now_ms_and_rfc3339().1,
            event: event.event,
            payload: event.payload,
        };
        if let Some(redactor) = hooks.redaction.as_ref() {
            redactor.redact_json_in_place(&mut record.payload);
        }
        record
    }

    pub(crate) fn redacted(mut self, hooks: &StoreHooks) -> Self {
        if let Some(redactor) = hooks.redaction.as_ref() {
            redactor.redact_json_in_place(&mut self.payload);
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct AdmitTask {
    pub task: TaskRecord,
    pub session: TaskSessionDescriptor,
    pub retry: Option<StoredTaskRetry>,
    pub initial_events: Vec<NewTaskControlEvent>,
    pub now_ms: i64,
}

impl AdmitTask {
    pub fn validate(&self) -> StoreResult<()> {
        if [
            &self.task.id,
            &self.task.session_id,
            &self.task.workspace_id,
            &self.session.workspace_root,
        ]
        .iter()
        .any(|identity| identity.trim().is_empty())
            || self.session.id != self.task.session_id
            || self.session.workspace_id != self.task.workspace_id
            || !self.task.status.is_active()
        {
            return Err(StoreError::InvalidInput(
                "invalid task admission identity or state".into(),
            ));
        }
        if self.retry.as_ref().is_some_and(|retry| {
            !is_digest(&retry.binding_digest) || !is_digest(&retry.request_fingerprint)
        }) {
            return Err(StoreError::InvalidInput("invalid task retry digest".into()));
        }
        Ok(())
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug)]
pub(crate) struct TaskBinding {
    pub task_id: String,
    pub request_fingerprint: String,
    pub expires_at_ms: i64,
}

pub(crate) enum RetryDecision {
    Create,
    Reuse,
    Conflict,
}

pub(crate) fn retry_decision(
    binding: &TaskBinding,
    task: &TaskRecord,
    fingerprint: &str,
    now_ms: i64,
) -> RetryDecision {
    if now_ms >= binding.expires_at_ms && !task.status.is_active() {
        RetryDecision::Create
    } else if binding.request_fingerprint == fingerprint {
        RetryDecision::Reuse
    } else {
        RetryDecision::Conflict
    }
}

pub(crate) fn status_event(
    task: &TaskRecord,
    previous_status: Option<TaskStatus>,
) -> NewTaskControlEvent {
    NewTaskControlEvent {
        event: if task.status == TaskStatus::Working
            && previous_status.is_none_or(|status| status == TaskStatus::Submitted)
        {
            "task.started"
        } else {
            task.status.event()
        }
        .into(),
        payload: task.resource(),
    }
}

/// Task facts and control events share one backend transaction owner.
/// Calls are synchronous like the underlying Memory/SQLite operations so ACP
/// notifications can commit a state change before publishing its projection.
pub trait TaskStore: Send + Sync {
    /// One persistent sequence also names non-task API control projections.
    fn next_control_event_id(&self) -> StoreResult<String>;
    fn admit_task(&self, request: AdmitTask) -> StoreResult<TaskAdmission<TaskRecord>>;
    fn get_task(&self, task_id: &str) -> StoreResult<TaskRecord>;
    fn list_tasks(
        &self,
        session_id: Option<&str>,
        workspace_id: Option<&str>,
    ) -> StoreResult<Vec<TaskRecord>>;
    fn get_task_session(&self, session_id: &str) -> StoreResult<TaskSessionDescriptor>;
    fn list_task_sessions(&self) -> StoreResult<Vec<TaskSessionDescriptor>>;
    fn update_task_session(
        &self,
        session_id: &str,
        update: TaskSessionUpdate,
    ) -> StoreResult<TaskSessionChange>;
    fn update_task(&self, task_id: &str, update: TaskUpdate) -> StoreResult<TaskChange>;
    fn append_task_event(
        &self,
        task_id: &str,
        event: NewTaskControlEvent,
    ) -> StoreResult<TaskControlEvent>;
    /// Accept input under the same lock that verifies its live task/session binding.
    fn append_task_input(
        &self,
        task_id: &str,
        session_id: &str,
        message: Value,
    ) -> StoreResult<TaskControlEvent>;
    fn task_events(
        &self,
        task_id: Option<&str>,
        session_id: Option<&str>,
    ) -> StoreResult<Vec<TaskControlEvent>>;
    fn task_message_view(&self, session_id: &str) -> StoreResult<TaskMessageView>;
    /// Hide old message projections without deleting their audit records.
    /// Messages committed after this snapshot remain visible.
    fn retain_task_messages(
        &self,
        session_id: &str,
        through_event_id: Option<&str>,
        retained_event_ids: &[String],
    ) -> StoreResult<()>;
    fn claim_task_execution(&self, task_id: &str) -> StoreResult<TaskExecutionLease>;
}
