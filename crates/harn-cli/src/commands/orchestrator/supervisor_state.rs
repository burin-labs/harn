use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::errors::OrchestratorError;

pub(crate) const SUPERVISOR_STATE_FILE: &str = "workflow-supervisor-state.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WorkflowSupervisorState {
    pub schema_version: u32,
    pub process: Option<WorkflowSupervisorProcess>,
    pub workflows: BTreeMap<String, WorkflowSupervisorWorkflow>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkflowSupervisorProcess {
    pub pid: u32,
    pub status: String,
    pub config_path: String,
    pub state_dir: String,
    pub bind: String,
    pub log_path: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkflowSupervisorWorkflow {
    pub workflow_id: String,
    pub status: String,
    pub reason: Option<String>,
    pub notification_hint: WorkflowSupervisorNotificationHint,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkflowSupervisorNotificationHint {
    pub kind: String,
    pub workflow_id: String,
    pub config_path: Option<String>,
    pub state_dir: String,
    pub resume_command: String,
    pub inspect_command: String,
}

impl WorkflowSupervisorState {
    pub(crate) fn normalized(mut self) -> Self {
        if self.schema_version == 0 {
            self.schema_version = 1;
        }
        self
    }
}

pub(crate) fn supervisor_state_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(SUPERVISOR_STATE_FILE)
}

pub(crate) fn read_supervisor_state(
    state_dir: &Path,
) -> Result<WorkflowSupervisorState, OrchestratorError> {
    let path = supervisor_state_path(state_dir);
    if !path.is_file() {
        return Ok(WorkflowSupervisorState {
            schema_version: 1,
            ..WorkflowSupervisorState::default()
        });
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str::<WorkflowSupervisorState>(&content)
        .map(WorkflowSupervisorState::normalized)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()).into())
}

pub(crate) fn write_supervisor_state(
    state_dir: &Path,
    state: &WorkflowSupervisorState,
) -> Result<(), OrchestratorError> {
    std::fs::create_dir_all(state_dir).map_err(|error| {
        format!(
            "failed to create supervisor state dir {}: {error}",
            state_dir.display()
        )
    })?;
    let path = supervisor_state_path(state_dir);
    let encoded = serde_json::to_string_pretty(state)
        .map_err(|error| format!("failed to encode supervisor state: {error}"))?;
    std::fs::write(&path, encoded)
        .map_err(|error| format!("failed to write {}: {error}", path.display()).into())
}

pub(crate) async fn apply_supervisor_state(
    state_dir: &Path,
) -> Result<WorkflowSupervisorState, OrchestratorError> {
    let state = read_supervisor_state(state_dir)?;
    for (workflow_id, workflow) in &state.workflows {
        match workflow.status.as_str() {
            "paused" => {
                harn_vm::pause(workflow_id).await.map_err(|error| {
                    format!("failed to pause workflow '{workflow_id}': {error}")
                })?;
            }
            "running" => {
                harn_vm::resume(workflow_id).await.map_err(|error| {
                    format!("failed to resume workflow '{workflow_id}': {error}")
                })?;
            }
            _ => {}
        }
    }
    Ok(state)
}

pub(crate) fn set_workflow_status(
    state: &mut WorkflowSupervisorState,
    config_path: Option<&Path>,
    state_dir: &Path,
    workflow_id: &str,
    status: &str,
    reason: Option<String>,
) -> Result<WorkflowSupervisorWorkflow, OrchestratorError> {
    let updated_at = now_rfc3339();
    let workflow = WorkflowSupervisorWorkflow {
        workflow_id: workflow_id.to_string(),
        status: status.to_string(),
        reason,
        notification_hint: workflow_notification_hint(config_path, state_dir, workflow_id),
        updated_at: updated_at.clone(),
    };
    state.schema_version = 1;
    state.updated_at = Some(updated_at);
    state
        .workflows
        .insert(workflow_id.to_string(), workflow.clone());
    Ok(workflow)
}

pub(crate) fn workflow_override<'a>(
    state: &'a WorkflowSupervisorState,
    workflow_id: &str,
) -> Option<&'a WorkflowSupervisorWorkflow> {
    state.workflows.get(workflow_id)
}

/// Current UTC time as RFC3339.
///
/// Was `Result<String, OrchestratorError>`; the shared helper is infallible
/// (see `harn_vm::clock::format_rfc3339`) so callers no longer propagate an
/// error that could not occur.
pub(crate) fn now_rfc3339() -> String {
    harn_vm::clock::system_now_rfc3339()
}

pub(crate) fn workflow_notification_hint(
    config_path: Option<&Path>,
    state_dir: &Path,
    workflow_id: &str,
) -> WorkflowSupervisorNotificationHint {
    let config_path_string = config_path.map(|path| path.display().to_string());
    let config_arg = config_path
        .map(|path| format!(" --config {}", path.display()))
        .unwrap_or_default();
    let state_dir = state_dir.display();
    WorkflowSupervisorNotificationHint {
        kind: "burin.resume_workflow".to_string(),
        workflow_id: workflow_id.to_string(),
        config_path: config_path_string,
        state_dir: state_dir.to_string(),
        resume_command: format!(
            "harn supervisor resume {workflow_id}{config_arg} --state-dir {state_dir} --json"
        ),
        inspect_command: format!(
            "harn supervisor inspect {workflow_id}{config_arg} --state-dir {state_dir} --json"
        ),
    }
}
