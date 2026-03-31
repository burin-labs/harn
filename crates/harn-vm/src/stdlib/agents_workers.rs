use super::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub(super) enum WorkerConfig {
    Workflow {
        graph: Box<WorkflowGraph>,
        artifacts: Vec<ArtifactRecord>,
        options: BTreeMap<String, VmValue>,
    },
    Stage {
        node: Box<crate::orchestration::WorkflowNode>,
        artifacts: Vec<ArtifactRecord>,
        transcript: Option<VmValue>,
    },
}

#[derive(Clone)]
pub(super) struct WorkerExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: Option<VmValue>,
    pub(super) artifacts: Vec<ArtifactRecord>,
}

#[derive(Clone, Default)]
pub(super) struct WorkerCarryPolicy {
    pub(super) artifact_mode: String,
    pub(super) context_policy: ContextPolicy,
    pub(super) transcript_policy: TranscriptPolicy,
    pub(super) resume_workflow: bool,
    pub(super) persist_state: bool,
}

pub(super) struct WorkerInit {
    pub(super) name: String,
    pub(super) task: String,
    pub(super) config: WorkerConfig,
    pub(super) wait: bool,
    pub(super) carry_policy: WorkerCarryPolicy,
}

pub(super) struct WorkerState {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) task: String,
    pub(super) status: String,
    pub(super) created_at: String,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) mode: String,
    pub(super) history: Vec<String>,
    pub(super) config: WorkerConfig,
    pub(super) handle: Option<tokio::task::JoinHandle<Result<WorkerExecutionResult, VmError>>>,
    pub(super) cancel_token: Arc<AtomicBool>,
    pub(super) latest_payload: Option<serde_json::Value>,
    pub(super) latest_error: Option<String>,
    pub(super) transcript: Option<VmValue>,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) parent_worker_id: Option<String>,
    pub(super) parent_stage_id: Option<String>,
    pub(super) child_run_id: Option<String>,
    pub(super) child_run_path: Option<String>,
    pub(super) carry_policy: WorkerCarryPolicy,
    pub(super) snapshot_path: String,
}

thread_local! {
    pub(super) static WORKER_REGISTRY: RefCell<BTreeMap<String, Rc<RefCell<WorkerState>>>> = const { RefCell::new(BTreeMap::new()) };
    static WORKER_COUNTER: Cell<u64> = const { Cell::new(0) };
}

pub(super) fn next_worker_id() -> String {
    WORKER_COUNTER.with(|counter| {
        let next = counter.get() + 1;
        counter.set(next);
        format!("worker_{}", uuid::Uuid::now_v7())
    })
}

pub(super) fn worker_id_from_value(value: &VmValue) -> Result<String, VmError> {
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        VmValue::Dict(map) => match map.get("id") {
            Some(VmValue::String(id)) => Ok(id.to_string()),
            Some(other) => Ok(other.display()),
            None => Err(VmError::Runtime(
                "agent handle dict is missing an id field".to_string(),
            )),
        },
        VmValue::TaskHandle(id) => Ok(id.clone()),
        _ => Err(VmError::Runtime(
            "expected agent handle or worker id".to_string(),
        )),
    }
}

pub(super) fn clone_worker_state(state: &WorkerState) -> serde_json::Value {
    serde_json::json!({
        "_type": "agent_handle",
        "id": state.id,
        "name": state.name,
        "task": state.task,
        "mode": state.mode,
        "status": state.status,
        "created_at": state.created_at,
        "started_at": state.started_at,
        "finished_at": state.finished_at,
        "history": state.history,
        "result": state.latest_payload,
        "error": state.latest_error,
        "artifact_count": state.artifacts.len(),
        "has_transcript": state.transcript.is_some(),
        "parent_worker_id": state.parent_worker_id,
        "parent_stage_id": state.parent_stage_id,
        "child_run_id": state.child_run_id,
        "child_run_path": state.child_run_path,
        "snapshot_path": state.snapshot_path,
    })
}

fn worker_bridge_metadata(state: &WorkerState) -> serde_json::Value {
    serde_json::json!({
        "task": state.task,
        "mode": state.mode,
        "created_at": state.created_at,
        "started_at": state.started_at,
        "finished_at": state.finished_at,
        "artifact_count": state.artifacts.len(),
        "has_transcript": state.transcript.is_some(),
        "parent_worker_id": state.parent_worker_id,
        "parent_stage_id": state.parent_stage_id,
        "child_run_id": state.child_run_id,
        "child_run_path": state.child_run_path,
        "snapshot_path": state.snapshot_path,
        "error": state.latest_error,
    })
}

pub(super) fn emit_worker_event(state: &WorkerState, status: &str) {
    if let Some(bridge) = crate::llm::current_host_bridge() {
        let metadata = worker_bridge_metadata(state);
        bridge.send_worker_update(&state.id, &state.name, status, metadata.clone());
        bridge.send_progress(
            "worker",
            &format!("{} {}", state.name, status),
            None,
            None,
            Some(serde_json::json!({
                "worker_id": state.id,
                "worker_name": state.name,
                "status": status,
                "metadata": metadata,
            })),
        );
    }
}

fn worker_state_dir() -> PathBuf {
    std::env::var("HARN_WORKER_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".harn/workers"))
}

pub(super) fn worker_snapshot_path(worker_id: &str) -> String {
    worker_state_dir()
        .join(format!("{worker_id}.json"))
        .to_string_lossy()
        .to_string()
}

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
    }
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
            let node: crate::orchestration::WorkflowNode = serde_json::from_value(
                value.get("node").cloned().unwrap_or_default(),
            )
            .map_err(|e| VmError::Runtime(format!("worker snapshot node parse error: {e}")))?;
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
        _ => Err(VmError::Runtime(
            "worker snapshot is missing a valid config mode".to_string(),
        )),
    }
}

pub(super) fn persist_worker_state_snapshot(state: &WorkerState) -> Result<(), VmError> {
    let payload = serde_json::json!({
        "_type": "worker_snapshot",
        "id": state.id,
        "name": state.name,
        "task": state.task,
        "status": state.status,
        "created_at": state.created_at,
        "started_at": state.started_at,
        "finished_at": state.finished_at,
        "mode": state.mode,
        "history": state.history,
        "config": worker_config_to_json(&state.config),
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
            "transcript_policy": state.carry_policy.transcript_policy,
            "resume_workflow": state.carry_policy.resume_workflow,
            "persist_state": state.carry_policy.persist_state,
        },
        "snapshot_path": state.snapshot_path,
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

pub(super) fn load_worker_state_snapshot(target: &str) -> Result<WorkerState, VmError> {
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
            transcript_policy: parse_transcript_policy(dict.get("transcript_policy"))?,
            resume_workflow: !matches!(dict.get("resume_workflow"), Some(VmValue::Bool(false))),
            persist_state: !matches!(dict.get("persist_state"), Some(VmValue::Bool(false))),
        }
    } else {
        WorkerCarryPolicy::default()
    };
    let config =
        worker_config_from_json(payload.get("config").unwrap_or(&serde_json::Value::Null))?;
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
        snapshot_path: path.to_string_lossy().to_string(),
    })
}

pub(super) fn worker_summary(state: &WorkerState) -> Result<VmValue, VmError> {
    to_vm(&clone_worker_state(state))
}

pub(super) fn with_worker_state<T>(
    worker_id: &str,
    f: impl FnOnce(Rc<RefCell<WorkerState>>) -> Result<T, VmError>,
) -> Result<T, VmError> {
    WORKER_REGISTRY.with(|registry| {
        let state = registry
            .borrow()
            .get(worker_id)
            .cloned()
            .ok_or_else(|| VmError::Runtime(format!("unknown worker: {worker_id}")))?;
        f(state)
    })
}

pub(super) fn parse_worker_carry_policy(
    dict: &BTreeMap<String, VmValue>,
) -> Result<WorkerCarryPolicy, VmError> {
    let carry = dict
        .get("carry")
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default();
    let artifact_mode = carry
        .get("artifact_mode")
        .or_else(|| carry.get("artifacts"))
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "inherit".to_string());
    let context_policy = parse_context_policy(carry.get("context_policy").or_else(|| {
        carry
            .get("artifacts")
            .filter(|value| value.as_dict().is_some())
    }))?;
    let mut transcript_policy =
        parse_transcript_policy(carry.get("transcript_policy").or_else(|| {
            carry
                .get("transcript")
                .filter(|value| value.as_dict().is_some())
        }))?;
    if transcript_policy.mode.is_none() {
        transcript_policy.mode = carry
            .get("transcript")
            .map(|value| value.display())
            .filter(|value| matches!(value.as_str(), "inherit" | "reset" | "fork"));
    }
    Ok(WorkerCarryPolicy {
        artifact_mode,
        context_policy,
        transcript_policy,
        resume_workflow: !matches!(carry.get("resume_workflow"), Some(VmValue::Bool(false))),
        persist_state: !matches!(carry.get("persist_state"), Some(VmValue::Bool(false))),
    })
}

pub(super) fn apply_worker_transcript_policy(
    transcript: Option<VmValue>,
    policy: &TranscriptPolicy,
) -> Option<VmValue> {
    crate::orchestration::apply_input_transcript_policy(transcript, policy)
}

pub(super) fn apply_worker_artifact_policy(
    artifacts: &[ArtifactRecord],
    policy: &WorkerCarryPolicy,
) -> Vec<ArtifactRecord> {
    if policy.artifact_mode == "none" {
        return Vec::new();
    }
    if policy.context_policy == ContextPolicy::default() {
        return artifacts.to_vec();
    }
    select_artifacts(artifacts.to_vec(), &policy.context_policy)
}

pub(super) fn parse_worker_config(value: &VmValue) -> Result<WorkerInit, VmError> {
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
    let carry_policy = parse_worker_carry_policy(dict)?;

    if let Some(graph_value) = dict.get("graph") {
        let graph = normalize_workflow_value(graph_value)?;
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
        });
    }

    let node_value = dict.get("node").ok_or_else(|| {
        VmError::Runtime("spawn_agent: config requires either graph or node".to_string())
    })?;
    let node: crate::orchestration::WorkflowNode =
        serde_json::from_value(crate::llm::vm_value_to_json(node_value))
            .map_err(|e| VmError::Runtime(format!("spawn_agent node: {e}")))?;
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
    })
}

async fn execute_worker_config(
    task: String,
    config: WorkerConfig,
) -> Result<WorkerExecutionResult, VmError> {
    match config {
        WorkerConfig::Workflow {
            mut graph,
            artifacts,
            mut options,
        } => {
            if let Some(parent_worker_id) = options
                .get("parent_worker_id")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
            {
                graph.metadata.insert(
                    "parent_worker_id".to_string(),
                    serde_json::json!(parent_worker_id),
                );
            }
            if let Some(parent_stage_id) = options
                .get("parent_stage_id")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
            {
                graph.metadata.insert(
                    "parent_stage_id".to_string(),
                    serde_json::json!(parent_stage_id),
                );
            }
            options.insert("delegated".to_string(), VmValue::Bool(true));
            let result = super::execute_workflow(task, *graph, artifacts, options).await?;
            let dict = result.as_dict().ok_or_else(|| {
                VmError::Runtime("workflow execution returned a non-dict result".to_string())
            })?;
            let transcript = dict.get("transcript").cloned();
            let artifacts = parse_artifact_list(dict.get("artifacts"))?;
            Ok(WorkerExecutionResult {
                payload: crate::llm::vm_value_to_json(&VmValue::Dict(Rc::new(dict.clone()))),
                transcript,
                artifacts,
            })
        }
        WorkerConfig::Stage {
            node,
            artifacts,
            transcript,
        } => {
            let (result, produced, next_transcript) = crate::orchestration::execute_stage_node(
                "delegated_worker",
                &node,
                &task,
                &artifacts,
                transcript,
            )
            .await?;
            Ok(WorkerExecutionResult {
                payload: serde_json::json!({
                    "status": "completed",
                    "mode": "stage",
                    "task": task,
                    "result": result,
                    "artifacts": produced,
                    "transcript": next_transcript.as_ref().map(crate::llm::vm_value_to_json),
                }),
                transcript: next_transcript,
                artifacts: produced,
            })
        }
    }
}

pub(super) fn spawn_worker_task(state: Rc<RefCell<WorkerState>>) {
    let (task, config, cancel_token) = {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker).ok();
        }
        emit_worker_event(&worker, "running");
        (
            worker.task.clone(),
            worker.config.clone(),
            worker.cancel_token.clone(),
        )
    };

    let state_for_task = state.clone();
    let handle = tokio::task::spawn_local(async move {
        if cancel_token.load(Ordering::SeqCst) {
            return Err(VmError::CategorizedError {
                message: "worker cancelled before start".to_string(),
                category: crate::value::ErrorCategory::Cancelled,
            });
        }

        let result = execute_worker_config(task, config).await;
        {
            let mut worker = state_for_task.borrow_mut();
            worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
            match &result {
                Ok(executed) => {
                    worker.status = "completed".to_string();
                    worker.latest_payload = Some(executed.payload.clone());
                    worker.latest_error = None;
                    worker.transcript = executed.transcript.clone();
                    worker.artifacts = executed.artifacts.clone();
                    worker.child_run_id = executed
                        .payload
                        .get("run")
                        .and_then(|run| run.get("id"))
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                    worker.child_run_path = executed
                        .payload
                        .get("path")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                    if worker.carry_policy.persist_state {
                        persist_worker_state_snapshot(&worker).ok();
                    }
                    emit_worker_event(&worker, "completed");
                }
                Err(error) => {
                    if matches!(
                        error,
                        VmError::CategorizedError {
                            category: crate::value::ErrorCategory::Cancelled,
                            ..
                        }
                    ) {
                        worker.status = "cancelled".to_string();
                    } else {
                        worker.status = "failed".to_string();
                    }
                    worker.latest_error = Some(error.to_string());
                    if worker.carry_policy.persist_state {
                        persist_worker_state_snapshot(&worker).ok();
                    }
                    emit_worker_event(&worker, &worker.status.clone());
                }
            }
        }
        result
    });

    state.borrow_mut().handle = Some(handle);
}

fn worker_result_artifact(
    node_id: &str,
    worker_id: &str,
    worker_name: &str,
    payload: &serde_json::Value,
    produced: &[ArtifactRecord],
    lineage: &[String],
) -> ArtifactRecord {
    let summary = payload
        .get("result")
        .or_else(|| payload.get("visible_text"))
        .or_else(|| payload.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: format!("{node_id}_worker_result_{}", uuid::Uuid::now_v7()),
        kind: "worker_result".to_string(),
        title: Some(format!("worker result {worker_name}")),
        text: if summary.is_empty() { None } else { Some(summary) },
        data: Some(serde_json::json!({
            "worker_id": worker_id,
            "worker_name": worker_name,
            "payload": payload,
            "produced_artifact_ids": produced.iter().map(|artifact| artifact.id.clone()).collect::<Vec<_>>(),
        })),
        source: Some(node_id.to_string()),
        created_at: uuid::Uuid::now_v7().to_string(),
        freshness: Some("fresh".to_string()),
        priority: Some(95),
        lineage: lineage.to_vec(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata: BTreeMap::from([
            ("worker_id".to_string(), serde_json::json!(worker_id)),
            ("worker_name".to_string(), serde_json::json!(worker_name)),
            ("delegated".to_string(), serde_json::json!(true)),
        ]),
    }
    .normalize()
}

pub(super) async fn execute_delegated_stage(
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let worker_id = next_worker_id();
    let worker_name = node
        .metadata
        .get("worker_name")
        .and_then(|value| value.as_str())
        .unwrap_or(node_id)
        .to_string();
    let mut stage_node = node.clone();
    stage_node.kind = "stage".to_string();
    let state = Rc::new(RefCell::new(WorkerState {
        id: worker_id.clone(),
        name: worker_name.clone(),
        task: task.to_string(),
        status: "running".to_string(),
        created_at: uuid::Uuid::now_v7().to_string(),
        started_at: uuid::Uuid::now_v7().to_string(),
        finished_at: None,
        mode: "delegated_stage".to_string(),
        history: vec![task.to_string()],
        config: WorkerConfig::Stage {
            node: Box::new(stage_node),
            artifacts: artifacts.to_vec(),
            transcript,
        },
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        latest_payload: None,
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: Some(node_id.to_string()),
        child_run_id: None,
        child_run_path: None,
        carry_policy: WorkerCarryPolicy {
            artifact_mode: "inherit".to_string(),
            context_policy: ContextPolicy::default(),
            transcript_policy: TranscriptPolicy::default(),
            resume_workflow: true,
            persist_state: true,
        },
        snapshot_path: worker_snapshot_path(&worker_id),
    }));
    {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
    }
    WORKER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(worker_id.clone(), state.clone());
    });
    spawn_worker_task(state.clone());
    let handle = state
        .borrow_mut()
        .handle
        .take()
        .ok_or_else(|| VmError::Runtime("delegated stage did not start".to_string()))?;
    let executed = handle
        .await
        .map_err(|error| VmError::Runtime(format!("delegated stage join error: {error}")))??;
    let mut result = executed.payload.clone();
    result["worker"] = clone_worker_state(&state.borrow());
    let mut produced = executed.artifacts.clone();
    for artifact in &mut produced {
        artifact
            .metadata
            .insert("worker_id".to_string(), serde_json::json!(worker_id));
        artifact.metadata.insert(
            "worker_name".to_string(),
            serde_json::json!(worker_name.clone()),
        );
        artifact
            .metadata
            .insert("delegated".to_string(), serde_json::json!(true));
    }
    produced.push(worker_result_artifact(
        node_id,
        &worker_id,
        &worker_name,
        &result,
        &executed.artifacts,
        &artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>(),
    ));
    Ok((result, produced, executed.transcript))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_snapshot_round_trip_preserves_resume_fields() {
        let dir = std::env::temp_dir().join(format!("harn-worker-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("HARN_WORKER_STATE_DIR", &dir) };

        let snapshot_path = worker_snapshot_path("worker_test");
        let state = WorkerState {
            id: "worker_test".to_string(),
            name: "worker".to_string(),
            task: "task".to_string(),
            status: "completed".to_string(),
            created_at: "created".to_string(),
            started_at: "started".to_string(),
            finished_at: Some("finished".to_string()),
            mode: "workflow".to_string(),
            history: vec!["task".to_string()],
            config: WorkerConfig::Stage {
                node: crate::orchestration::WorkflowNode {
                    kind: "stage".to_string(),
                    ..Default::default()
                },
                artifacts: Vec::new(),
                transcript: Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                    "_type".to_string(),
                    VmValue::String(Rc::from("transcript")),
                )])))),
            },
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
            latest_payload: Some(serde_json::json!({"status": "completed"})),
            latest_error: None,
            transcript: Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                "_type".to_string(),
                VmValue::String(Rc::from("transcript")),
            )])))),
            artifacts: vec![ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "artifact_1".to_string(),
                kind: "summary".to_string(),
                title: Some("summary".to_string()),
                text: Some("done".to_string()),
                data: None,
                source: Some("test".to_string()),
                created_at: "now".to_string(),
                freshness: Some("fresh".to_string()),
                priority: Some(60),
                lineage: Vec::new(),
                relevance: Some(1.0),
                estimated_tokens: Some(1),
                stage: Some("stage".to_string()),
                metadata: BTreeMap::new(),
            }],
            parent_worker_id: Some("parent".to_string()),
            parent_stage_id: Some("stage".to_string()),
            child_run_id: Some("run_1".to_string()),
            child_run_path: Some(".harn-runs/run_1.json".to_string()),
            carry_policy: WorkerCarryPolicy {
                artifact_mode: "none".to_string(),
                context_policy: ContextPolicy::default(),
                transcript_policy: TranscriptPolicy {
                    mode: Some("reset".to_string()),
                    ..Default::default()
                },
                resume_workflow: false,
                persist_state: true,
            },
            snapshot_path: snapshot_path.clone(),
        };

        persist_worker_state_snapshot(&state).unwrap();
        let loaded = load_worker_state_snapshot(&snapshot_path).unwrap();
        assert_eq!(loaded.id, "worker_test");
        assert_eq!(loaded.child_run_id.as_deref(), Some("run_1"));
        assert_eq!(
            loaded.child_run_path.as_deref(),
            Some(".harn-runs/run_1.json")
        );
        assert_eq!(loaded.carry_policy.artifact_mode, "none");
        assert!(!loaded.carry_policy.resume_workflow);
        assert_eq!(
            loaded.carry_policy.transcript_policy.mode.as_deref(),
            Some("reset")
        );

        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("HARN_WORKER_STATE_DIR") };
    }

    #[test]
    fn artifact_carry_policy_can_drop_all_artifacts() {
        let policy = WorkerCarryPolicy {
            artifact_mode: "none".to_string(),
            ..Default::default()
        };
        let artifacts = vec![ArtifactRecord {
            kind: "summary".to_string(),
            ..Default::default()
        }];
        let selected = apply_worker_artifact_policy(&artifacts, &policy);
        assert!(selected.is_empty());
    }
}
