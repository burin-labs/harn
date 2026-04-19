use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::super::{parse_artifact_list, parse_context_policy, SubAgentRunSpec};
use super::audit::parse_worker_audit;
use super::bridge::worker_snapshot_path;
use super::policy::{
    parse_worker_carry_policy, parse_worker_policy_value, resolve_worker_policy,
    worker_policy_value,
};
use super::{
    WorkerCarryPolicy, WorkerConfig, WorkerExecutionProfile, WorkerInit, WorkerRequestRecord,
    WorkerState,
};
use crate::orchestration::{ArtifactRecord, MutationSessionRecord, WorkflowGraph};
use crate::value::{VmError, VmValue};

fn worker_config_to_json(config: &WorkerConfig) -> serde_json::Value {
    match config {
        WorkerConfig::Workflow {
            graph,
            artifacts,
            options,
        } => serde_json::json!({
            "mode": "workflow",
            "graph": graph,
            "artifacts": artifacts,
            "options": options.iter().map(|(key, value)| (key.clone(), crate::llm::vm_value_to_json(value))).collect::<BTreeMap<_, _>>(),
        }),
        WorkerConfig::Stage {
            node,
            artifacts,
            transcript,
        } => serde_json::json!({
            "mode": "stage",
            "node": node,
            "artifacts": artifacts,
            "transcript": transcript.as_ref().map(crate::llm::vm_value_to_json),
        }),
        WorkerConfig::SubAgent { spec } => serde_json::json!({
            "mode": "sub_agent",
            "spec": sub_agent_spec_to_json(spec),
        }),
    }
}

fn sub_agent_spec_to_json(spec: &SubAgentRunSpec) -> serde_json::Value {
    serde_json::json!({
        "name": &spec.name,
        "task": &spec.task,
        "system": &spec.system,
        "options": spec
            .options
            .iter()
            .map(|(key, value)| (key.clone(), crate::llm::vm_value_to_json(value)))
            .collect::<BTreeMap<_, _>>(),
        "returns_schema": spec
            .returns_schema
            .as_ref()
            .map(crate::llm::vm_value_to_json),
        "session_id": &spec.session_id,
        "parent_session_id": &spec.parent_session_id,
    })
}

fn sub_agent_spec_from_json(value: &serde_json::Value) -> Result<SubAgentRunSpec, VmError> {
    let dict = value.as_object().ok_or_else(|| {
        VmError::Runtime("worker snapshot sub-agent spec must be an object".to_string())
    })?;
    let options = dict
        .get("options")
        .and_then(|options| options.as_object())
        .map(|options| {
            options
                .iter()
                .map(|(key, value)| (key.clone(), crate::stdlib::json_to_vm_value(value)))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    Ok(SubAgentRunSpec {
        name: dict
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        task: dict
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        system: dict
            .get("system")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        options,
        returns_schema: dict
            .get("returns_schema")
            .map(crate::stdlib::json_to_vm_value),
        session_id: dict
            .get("session_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        parent_session_id: dict
            .get("parent_session_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
    })
}

fn worker_config_from_json(value: &serde_json::Value) -> Result<WorkerConfig, VmError> {
    let mode = value
        .get("mode")
        .and_then(|mode| mode.as_str())
        .unwrap_or_default();
    match mode {
        "workflow" => {
            let graph: WorkflowGraph = serde_json::from_value(
                value.get("graph").cloned().unwrap_or_default(),
            )
            .map_err(|e| VmError::Runtime(format!("worker snapshot graph parse error: {e}")))?;
            let artifacts: Vec<ArtifactRecord> =
                serde_json::from_value(value.get("artifacts").cloned().unwrap_or_default())
                    .map_err(|e| {
                        VmError::Runtime(format!("worker snapshot artifacts parse error: {e}"))
                    })?;
            let options = value
                .get("options")
                .and_then(|options| options.as_object())
                .map(|options| {
                    options
                        .iter()
                        .map(|(key, value)| (key.clone(), crate::stdlib::json_to_vm_value(value)))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            Ok(WorkerConfig::Workflow {
                graph: Box::new(graph),
                artifacts,
                options,
            })
        }
        "stage" => {
            let node = crate::orchestration::parse_workflow_node_json(
                value.get("node").cloned().unwrap_or_default(),
                "worker snapshot node",
            )?;
            let artifacts: Vec<ArtifactRecord> =
                serde_json::from_value(value.get("artifacts").cloned().unwrap_or_default())
                    .map_err(|e| {
                        VmError::Runtime(format!("worker snapshot artifacts parse error: {e}"))
                    })?;
            let transcript = value.get("transcript").map(crate::stdlib::json_to_vm_value);
            Ok(WorkerConfig::Stage {
                node: Box::new(node),
                artifacts,
                transcript,
            })
        }
        "sub_agent" => {
            let spec =
                sub_agent_spec_from_json(value.get("spec").unwrap_or(&serde_json::Value::Null))
                    .map_err(|e| {
                        VmError::Runtime(format!("worker snapshot sub-agent parse error: {e}"))
                    })?;
            Ok(WorkerConfig::SubAgent {
                spec: Box::new(spec),
            })
        }
        _ => Err(VmError::Runtime(
            "worker snapshot is missing a valid config mode".to_string(),
        )),
    }
}

pub(in super::super) fn persist_worker_state_snapshot(state: &WorkerState) -> Result<(), VmError> {
    let payload = serde_json::json!({
        "_type": "worker_snapshot",
        "id": state.id,
        "name": state.name,
        "task": state.task,
        "status": state.status,
        "created_at": state.created_at,
        "started_at": state.started_at,
        "finished_at": state.finished_at,
        "awaiting_started_at": state.awaiting_started_at,
        "mode": state.mode,
        "history": state.history,
        "config": worker_config_to_json(&state.config),
        "request": state.request,
        "latest_payload": state.latest_payload,
        "latest_error": state.latest_error,
        "transcript": state.transcript.as_ref().map(crate::llm::vm_value_to_json),
        "artifacts": state.artifacts,
        "parent_worker_id": state.parent_worker_id,
        "parent_stage_id": state.parent_stage_id,
        "child_run_id": state.child_run_id,
        "child_run_path": state.child_run_path,
        "carry_policy": {
            "artifact_mode": state.carry_policy.artifact_mode,
            "context_policy": state.carry_policy.context_policy,
            "resume_workflow": state.carry_policy.resume_workflow,
            "persist_state": state.carry_policy.persist_state,
            "retriggerable": state.carry_policy.retriggerable,
            "policy": state.carry_policy.policy,
        },
        "execution": state.execution,
        "snapshot_path": state.snapshot_path,
        "audit": state.audit,
    });
    let path = PathBuf::from(&state.snapshot_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("worker snapshot mkdir error: {e}")))?;
    }
    let json = serde_json::to_string_pretty(&payload)
        .map_err(|e| VmError::Runtime(format!("worker snapshot encode error: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| VmError::Runtime(format!("worker snapshot write error: {e}")))?;
    Ok(())
}

pub(in super::super) fn load_worker_state_snapshot(target: &str) -> Result<WorkerState, VmError> {
    let path = if target.ends_with(".json") || target.contains('/') {
        PathBuf::from(target)
    } else {
        PathBuf::from(worker_snapshot_path(target))
    };
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| VmError::Runtime(format!("worker snapshot read error: {e}")))?;
    let payload: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| VmError::Runtime(format!("worker snapshot parse error: {e}")))?;
    let carry_policy = if let Some(carry_value) = payload.get("carry_policy") {
        let value = crate::stdlib::json_to_vm_value(carry_value);
        let dict = value.as_dict().cloned().unwrap_or_default();
        let artifact_mode = dict
            .get("artifact_mode")
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "inherit".to_string());
        WorkerCarryPolicy {
            artifact_mode,
            context_policy: parse_context_policy(dict.get("context_policy"))?,
            resume_workflow: !matches!(dict.get("resume_workflow"), Some(VmValue::Bool(false))),
            persist_state: !matches!(dict.get("persist_state"), Some(VmValue::Bool(false))),
            retriggerable: matches!(dict.get("retriggerable"), Some(VmValue::Bool(true))),
            policy: worker_policy_value(dict.get("policy"))
                .map(parse_worker_policy_value)
                .transpose()?,
        }
    } else {
        WorkerCarryPolicy::default()
    };
    let config =
        worker_config_from_json(payload.get("config").unwrap_or(&serde_json::Value::Null))?;
    let audit: MutationSessionRecord =
        serde_json::from_value(payload.get("audit").cloned().unwrap_or_default())
            .unwrap_or_default();
    let execution: WorkerExecutionProfile =
        serde_json::from_value(payload.get("execution").cloned().unwrap_or_default())
            .map_err(|e| VmError::Runtime(format!("worker snapshot execution parse error: {e}")))?;
    let request: WorkerRequestRecord =
        serde_json::from_value(payload.get("request").cloned().unwrap_or_default())
            .unwrap_or_else(|_| WorkerRequestRecord::default());
    let status = payload
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("interrupted");
    let normalized_status = if status == "running" {
        "interrupted".to_string()
    } else {
        status.to_string()
    };
    Ok(WorkerState {
        id: payload
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        name: payload
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("worker")
            .to_string(),
        task: payload
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        status: normalized_status,
        created_at: payload
            .get("created_at")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        started_at: payload
            .get("started_at")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        finished_at: payload
            .get("finished_at")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        awaiting_started_at: payload
            .get("awaiting_started_at")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        awaiting_since: None,
        mode: payload
            .get("mode")
            .and_then(|value| value.as_str())
            .unwrap_or("workflow")
            .to_string(),
        history: payload
            .get("history")
            .and_then(|value| value.as_array())
            .map(|history| {
                history
                    .iter()
                    .filter_map(|value| value.as_str().map(|value| value.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        config,
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        request,
        latest_payload: payload.get("latest_payload").cloned(),
        latest_error: payload
            .get("latest_error")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        transcript: payload
            .get("transcript")
            .map(crate::stdlib::json_to_vm_value),
        artifacts: payload
            .get("artifacts")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| VmError::Runtime(format!("worker snapshot artifacts parse error: {e}")))?
            .unwrap_or_default(),
        parent_worker_id: payload
            .get("parent_worker_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        parent_stage_id: payload
            .get("parent_stage_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        child_run_id: payload
            .get("child_run_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        child_run_path: payload
            .get("child_run_path")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        carry_policy,
        execution,
        snapshot_path: path.to_string_lossy().into_owned(),
        audit: audit.normalize(),
    })
}

pub(in super::super) fn parse_worker_execution_profile(
    value: Option<&VmValue>,
) -> Result<WorkerExecutionProfile, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("worker execution parse error: {e}"))),
        None => Ok(WorkerExecutionProfile::default()),
    }
}

pub(super) fn parse_execution_profile_json(
    value: Option<&serde_json::Value>,
) -> Result<WorkerExecutionProfile, VmError> {
    match value {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| VmError::Runtime(format!("worker execution parse error: {e}"))),
        None => Ok(WorkerExecutionProfile::default()),
    }
}

pub(in super::super) fn parse_worker_config(value: &VmValue) -> Result<WorkerInit, VmError> {
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime("spawn_agent: config must be a dict".to_string()))?;
    let task = dict
        .get("task")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("spawn_agent: config.task is required".to_string()))?;
    let name = dict
        .get("name")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "worker".to_string());
    let wait = matches!(dict.get("wait"), Some(VmValue::Bool(true)));
    let mut carry_policy = parse_worker_carry_policy(dict)?;
    carry_policy.policy = resolve_worker_policy(dict)?;
    let execution = parse_worker_execution_profile(dict.get("execution"))?;
    let audit = parse_worker_audit(dict)?;

    if let Some(graph_value) = dict.get("graph") {
        let graph = crate::orchestration::normalize_workflow_value(graph_value)?;
        let artifacts = parse_artifact_list(dict.get("artifacts"))?;
        let options = dict
            .get("options")
            .and_then(|value| value.as_dict())
            .cloned()
            .unwrap_or_default();
        return Ok(WorkerInit {
            name,
            task,
            config: WorkerConfig::Workflow {
                graph: Box::new(graph),
                artifacts,
                options,
            },
            wait,
            carry_policy,
            execution,
            audit,
        });
    }

    let node_value = dict.get("node").ok_or_else(|| {
        VmError::Runtime("spawn_agent: config requires either graph or node".to_string())
    })?;
    let node = crate::orchestration::parse_workflow_node_json(
        crate::llm::vm_value_to_json(node_value),
        "spawn_agent node",
    )?;
    let artifacts = parse_artifact_list(dict.get("artifacts"))?;
    let transcript = dict.get("transcript").cloned();
    Ok(WorkerInit {
        name,
        task,
        config: WorkerConfig::Stage {
            node: Box::new(node),
            artifacts,
            transcript,
        },
        wait,
        carry_policy,
        execution,
        audit,
    })
}
