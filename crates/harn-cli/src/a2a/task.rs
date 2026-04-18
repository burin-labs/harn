use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

/// The lifecycle states of an A2A task, aligned with the A2A protocol spec v1.0.
/// See: https://a2a-protocol.org/latest/specification/
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum TaskStatus {
    Submitted,
    Working,
    Completed,
    Failed,
    Cancelled,
    /// The remote agent rejected the task (e.g., unsupported capability).
    Rejected,
    /// The agent needs additional input from the caller to continue.
    InputRequired,
    /// The agent needs authentication/authorization from the caller.
    AuthRequired,
}

impl TaskStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Submitted => "submitted",
            TaskStatus::Working => "working",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Rejected => "rejected",
            TaskStatus::InputRequired => "input-required",
            TaskStatus::AuthRequired => "auth-required",
        }
    }

    /// Terminal states: task processing is complete (no further updates).
    pub(super) fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Rejected
        )
    }

    /// Interrupted states: task is paused, waiting for external input.
    #[allow(dead_code)]
    pub(super) fn is_interrupted(&self) -> bool {
        matches!(self, TaskStatus::InputRequired | TaskStatus::AuthRequired)
    }
}

/// A part of a message (text, file, data, etc.).
/// For now we store parts as opaque JSON values.
type MessagePart = serde_json::Value;

/// An A2A message in the task history (v1.0: includes unique id).
#[derive(Clone, Debug)]
pub(super) struct TaskMessage {
    pub(super) id: String,
    pub(super) role: String,
    pub(super) parts: Vec<MessagePart>,
}

/// An artifact produced by the agent during task execution.
#[derive(Clone, Debug)]
pub(super) struct Artifact {
    pub(super) id: String,
    pub(super) title: Option<String>,
    pub(super) description: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) parts: Vec<MessagePart>,
}

impl Artifact {
    pub(super) fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "id": self.id,
            "parts": self.parts,
        });
        if let Some(ref t) = self.title {
            obj["title"] = serde_json::Value::String(t.clone());
        }
        if let Some(ref d) = self.description {
            obj["description"] = serde_json::Value::String(d.clone());
        }
        if let Some(ref m) = self.mime_type {
            obj["mimeType"] = serde_json::Value::String(m.clone());
        }
        obj
    }
}

/// Persistent state for a single A2A task.
#[derive(Clone, Debug)]
pub(super) struct TaskState {
    pub(super) id: String,
    pub(super) context_id: Option<String>,
    pub(super) status: TaskStatus,
    pub(super) history: Vec<TaskMessage>,
    pub(super) artifacts: Vec<Artifact>,
}

impl TaskState {
    pub(super) fn to_json(&self) -> serde_json::Value {
        let history: Vec<serde_json::Value> = self
            .history
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "role": m.role,
                    "parts": m.parts,
                })
            })
            .collect();
        let artifacts: Vec<serde_json::Value> =
            self.artifacts.iter().map(|a| a.to_json()).collect();
        let mut obj = serde_json::json!({
            "id": self.id,
            "status": {"state": self.status.as_str()},
            "history": history,
            "artifacts": artifacts,
        });
        if let Some(ref cid) = self.context_id {
            obj["contextId"] = serde_json::Value::String(cid.clone());
        }
        obj
    }

    /// A summary representation for list operations.
    pub(super) fn to_summary_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "id": self.id,
            "status": {"state": self.status.as_str()},
        });
        if let Some(ref cid) = self.context_id {
            obj["contextId"] = serde_json::Value::String(cid.clone());
        }
        obj
    }
}

/// Thread-safe map of task-id -> TaskState.
pub(super) type TaskStore = Arc<Mutex<HashMap<String, TaskState>>>;

/// Create a new task in the store with `submitted` status, returning its id.
pub(super) fn create_task(
    store: &TaskStore,
    task_text: &str,
    context_id: Option<String>,
) -> String {
    let task_id = Uuid::now_v7().to_string();
    let message_id = Uuid::now_v7().to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id,
        status: TaskStatus::Submitted,
        history: vec![TaskMessage {
            id: message_id,
            role: "user".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": task_text})],
        }],
        artifacts: Vec::new(),
    };
    store.lock().unwrap().insert(task_id.clone(), task);
    task_id
}

/// Transition a task to `working`.
pub(super) fn mark_task_working(store: &TaskStore, task_id: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Working;
    }
}

/// Check whether a task has been cancelled (used to skip execution).
pub(super) fn is_task_cancelled(store: &TaskStore, task_id: &str) -> bool {
    store
        .lock()
        .unwrap()
        .get(task_id)
        .is_some_and(|t| t.status == TaskStatus::Cancelled)
}

/// Complete a task with the agent's output.
pub(super) fn complete_task(store: &TaskStore, task_id: &str, output: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Completed;
        let message_id = Uuid::now_v7().to_string();
        task.history.push(TaskMessage {
            id: message_id,
            role: "agent".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": output.trim_end()})],
        });
    }
}

/// Mark a task as failed with an error message.
pub(super) fn fail_task(store: &TaskStore, task_id: &str, error: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Failed;
        let message_id = Uuid::now_v7().to_string();
        task.history.push(TaskMessage {
            id: message_id,
            role: "agent".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": error})],
        });
    }
}

/// Cancel a task by id. Returns the updated task JSON on success, or an error
/// message on failure.
pub(super) fn cancel_task(store: &TaskStore, task_id: &str) -> Result<serde_json::Value, String> {
    let mut map = store.lock().unwrap();
    let task = map
        .get_mut(task_id)
        .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
    if task.status.is_terminal() {
        return Err(format!(
            "TaskNotCancelableError: task {} is in terminal state '{}'",
            task_id,
            task.status.as_str()
        ));
    }
    task.status = TaskStatus::Cancelled;
    Ok(task.to_json())
}

/// List tasks with cursor-based pagination. `cursor` is the task id to
/// start after; `limit` defaults to 50.
pub(super) fn list_tasks(
    store: &TaskStore,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> serde_json::Value {
    let map = store.lock().unwrap();
    let limit = limit.unwrap_or(50);

    // Sort tasks by id for stable pagination ordering
    let mut task_ids: Vec<&String> = map.keys().collect();
    task_ids.sort();

    // If cursor is provided, skip to after it
    let start_idx = if let Some(cursor_id) = cursor {
        task_ids
            .iter()
            .position(|id| id.as_str() == cursor_id)
            .map(|pos| pos + 1)
            .unwrap_or(0)
    } else {
        0
    };

    let tasks: Vec<serde_json::Value> = task_ids
        .iter()
        .skip(start_idx)
        .take(limit)
        .filter_map(|id| map.get(id.as_str()).map(|t| t.to_summary_json()))
        .collect();

    let next_cursor = if start_idx + limit < task_ids.len() {
        task_ids.get(start_idx + limit - 1).map(|id| id.as_str())
    } else {
        None
    };

    let mut result = serde_json::json!({
        "tasks": tasks,
    });
    if let Some(nc) = next_cursor {
        result["nextCursor"] = serde_json::Value::String(nc.to_string());
    }
    result
}
