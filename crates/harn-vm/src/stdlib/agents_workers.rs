use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
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
    SubAgent {
        spec: Box<SubAgentRunSpec>,
    },
}

#[derive(Clone)]
pub(super) struct WorkerExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: Option<VmValue>,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) execution: WorkerExecutionProfile,
}

#[derive(Clone, Default)]
pub(super) struct WorkerCarryPolicy {
    pub(super) artifact_mode: String,
    pub(super) context_policy: ContextPolicy,
    pub(super) resume_workflow: bool,
    pub(super) persist_state: bool,
    /// Capability policy scoped to this worker. Pushed onto the policy stack
    /// during execution and popped when the worker completes.
    pub(super) policy: Option<CapabilityPolicy>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct WorkerWorktreeSpec {
    pub(super) repo: String,
    pub(super) path: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) base_ref: Option<String>,
    pub(super) cleanup: Option<String>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct WorkerExecutionProfile {
    pub(super) cwd: Option<String>,
    pub(super) env: BTreeMap<String, String>,
    pub(super) worktree: Option<WorkerWorktreeSpec>,
}

pub(super) struct WorkerInit {
    pub(super) name: String,
    pub(super) task: String,
    pub(super) config: WorkerConfig,
    pub(super) wait: bool,
    pub(super) carry_policy: WorkerCarryPolicy,
    pub(super) execution: WorkerExecutionProfile,
    pub(super) audit: MutationSessionRecord,
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
    pub(super) execution: WorkerExecutionProfile,
    pub(super) snapshot_path: String,
    pub(super) audit: MutationSessionRecord,
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
        "execution": state.execution,
        "snapshot_path": state.snapshot_path,
        "audit": state.audit,
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
        "execution": state.execution,
        "snapshot_path": state.snapshot_path,
        "audit": state.audit,
        "error": state.latest_error,
    })
}

pub(super) fn emit_worker_event(state: &WorkerState, status: &str) {
    if let Some(bridge) = crate::llm::current_host_bridge() {
        let metadata = worker_bridge_metadata(state);
        bridge.send_worker_update(
            &state.id,
            &state.name,
            status,
            metadata.clone(),
            Some(&state.audit),
        );
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
        .into_owned()
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
            "resume_workflow": state.carry_policy.resume_workflow,
            "persist_state": state.carry_policy.persist_state,
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
            resume_workflow: !matches!(dict.get("resume_workflow"), Some(VmValue::Bool(false))),
            persist_state: !matches!(dict.get("persist_state"), Some(VmValue::Bool(false))),
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
        execution,
        snapshot_path: path.to_string_lossy().into_owned(),
        audit: audit.normalize(),
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

    Ok(WorkerCarryPolicy {
        artifact_mode,
        context_policy,
        resume_workflow: !matches!(carry.get("resume_workflow"), Some(VmValue::Bool(false))),
        persist_state: !matches!(carry.get("persist_state"), Some(VmValue::Bool(false))),
        policy: None,
    })
}

fn parse_worker_policy_value(value: &VmValue) -> Result<CapabilityPolicy, VmError> {
    let json = crate::llm::helpers::vm_value_to_json(value);
    serde_json::from_value(json)
        .map_err(|e| VmError::Runtime(format!("spawn_agent: policy parse error: {e}")))
}

fn worker_policy_value(value: Option<&VmValue>) -> Option<&VmValue> {
    value.filter(|value| !matches!(value, VmValue::Nil))
}

fn parse_worker_tools_policy(value: Option<&VmValue>) -> Result<Option<CapabilityPolicy>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let tools = match value {
        VmValue::List(list) => list,
        _ => {
            return Err(VmError::Runtime(
                "spawn_agent: tools shorthand must be a list of strings".to_string(),
            ))
        }
    };
    let mut allowed = Vec::new();
    for tool in tools.iter() {
        let name = match tool {
            VmValue::String(text) => text.trim().to_string(),
            _ => {
                return Err(VmError::Runtime(
                    "spawn_agent: tools shorthand must be a list of strings".to_string(),
                ))
            }
        };
        if !name.is_empty() && !allowed.contains(&name) {
            allowed.push(name);
        }
    }
    if allowed.is_empty() {
        return Err(VmError::Runtime(
            "spawn_agent: tools shorthand must include at least one tool name".to_string(),
        ));
    }
    Ok(Some(CapabilityPolicy {
        tools: allowed,
        ..Default::default()
    }))
}

fn resolve_worker_policy(
    dict: &BTreeMap<String, VmValue>,
) -> Result<Option<CapabilityPolicy>, VmError> {
    let carry = dict
        .get("carry")
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default();
    let explicit = carry
        .get("policy")
        .or_else(|| dict.get("policy"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(parse_worker_policy_value)
        .transpose()?;
    let tools = parse_worker_tools_policy(carry.get("tools").or_else(|| dict.get("tools")))?;
    let requested = match (explicit, tools) {
        (Some(policy), Some(tool_policy)) => Some(
            policy
                .intersect(&tool_policy)
                .map_err(|e| VmError::Runtime(format!("spawn_agent: {e}")))?,
        ),
        (Some(policy), None) => Some(policy),
        (None, Some(tool_policy)) => Some(tool_policy),
        (None, None) => None,
    };
    resolve_inherited_worker_policy(requested)
}

fn parse_worker_audit(dict: &BTreeMap<String, VmValue>) -> Result<MutationSessionRecord, VmError> {
    let audit_value = dict
        .get("audit")
        .cloned()
        .unwrap_or_else(|| VmValue::Dict(Rc::new(BTreeMap::new())));
    let parent_session = crate::orchestration::current_mutation_session();
    let mut audit: MutationSessionRecord =
        serde_json::from_value(crate::llm::vm_value_to_json(&audit_value))
            .map_err(|e| VmError::Runtime(format!("worker audit parse error: {e}")))?;
    if audit.parent_session_id.is_none() {
        audit.parent_session_id = parent_session
            .as_ref()
            .map(|session| session.session_id.clone());
    }
    if audit.run_id.is_none() {
        audit.run_id = parent_session
            .as_ref()
            .and_then(|session| session.run_id.clone());
    }
    if audit.execution_kind.is_none() {
        audit.execution_kind = Some("worker".to_string());
    }
    if audit.mutation_scope.is_empty() {
        audit.mutation_scope = parent_session
            .as_ref()
            .map(|session| session.mutation_scope.clone())
            .unwrap_or_else(|| "read_only".to_string());
    }
    if audit.approval_policy.is_none() {
        audit.approval_policy = parent_session
            .as_ref()
            .and_then(|session| session.approval_policy.clone());
    }
    Ok(audit.normalize())
}

pub(super) fn parse_worker_execution_profile(
    value: Option<&VmValue>,
) -> Result<WorkerExecutionProfile, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("worker execution parse error: {e}"))),
        None => Ok(WorkerExecutionProfile::default()),
    }
}

pub(super) fn resolve_inherited_worker_policy(
    requested: Option<CapabilityPolicy>,
) -> Result<Option<CapabilityPolicy>, VmError> {
    let parent = crate::orchestration::current_execution_policy();
    match (parent, requested) {
        (Some(parent), Some(requested)) => {
            Ok(Some(parent.intersect(&requested).map_err(|e| {
                VmError::Runtime(format!("spawn_agent: {e}"))
            })?))
        }
        (Some(parent), None) => Ok(Some(parent)),
        (None, Some(requested)) => Ok(Some(requested)),
        (None, None) => Ok(None),
    }
}

pub(super) fn inherited_worker_audit(execution_kind: &str) -> MutationSessionRecord {
    let parent_session = crate::orchestration::current_mutation_session();
    MutationSessionRecord {
        parent_session_id: parent_session
            .as_ref()
            .map(|session| session.session_id.clone()),
        run_id: parent_session
            .as_ref()
            .and_then(|session| session.run_id.clone()),
        execution_kind: Some(execution_kind.to_string()),
        mutation_scope: parent_session
            .as_ref()
            .map(|session| session.mutation_scope.clone())
            .unwrap_or_else(|| "read_only".to_string()),
        approval_policy: parent_session
            .as_ref()
            .and_then(|session| session.approval_policy.clone()),
        ..Default::default()
    }
    .normalize()
}

fn parse_execution_profile_json(
    value: Option<&serde_json::Value>,
) -> Result<WorkerExecutionProfile, VmError> {
    match value {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| VmError::Runtime(format!("worker execution parse error: {e}"))),
        None => Ok(WorkerExecutionProfile::default()),
    }
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
    let mut carry_policy = parse_worker_carry_policy(dict)?;
    carry_policy.policy = resolve_worker_policy(dict)?;
    let execution = parse_worker_execution_profile(dict.get("execution"))?;
    let audit = parse_worker_audit(dict)?;

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

fn execution_record(profile: &WorkerExecutionProfile) -> crate::orchestration::RunExecutionRecord {
    let mut record = crate::orchestration::RunExecutionRecord {
        cwd: profile.cwd.clone(),
        source_dir: None,
        env: profile.env.clone(),
        adapter: None,
        repo_path: None,
        worktree_path: None,
        branch: None,
        base_ref: None,
        cleanup: None,
    };
    if let Some(worktree) = &profile.worktree {
        record.adapter = Some("worktree".to_string());
        record.repo_path = Some(worktree.repo.clone());
        record.worktree_path = worktree.path.clone().or_else(|| profile.cwd.clone());
        record.branch = worktree.branch.clone();
        record.base_ref = worktree.base_ref.clone();
        record.cleanup = worktree.cleanup.clone();
    }
    record
}

fn infer_worktree_path(worker_id: &str, spec: &WorkerWorktreeSpec) -> Result<String, VmError> {
    if let Some(path) = &spec.path {
        return Ok(path.clone());
    }
    let repo_name = PathBuf::from(&spec.repo)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo")
        .to_string();
    let base_dir = crate::stdlib::process::current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(crate::runtime_paths::worktree_root(&base_dir)
        .join(repo_name)
        .join(worker_id)
        .display()
        .to_string())
}

fn ensure_worker_worktree(
    worker_id: &str,
    profile: &mut WorkerExecutionProfile,
) -> Result<(), VmError> {
    let Some(spec) = profile.worktree.as_mut() else {
        return Ok(());
    };
    if spec.repo.trim().is_empty() {
        return Err(VmError::Runtime(
            "worker execution.worktree.repo must not be empty".to_string(),
        ));
    }
    let path = infer_worktree_path(worker_id, spec)?;
    let base_ref = spec.base_ref.clone().unwrap_or_else(|| "HEAD".to_string());
    let branch = spec
        .branch
        .clone()
        .unwrap_or_else(|| format!("harn-{worker_id}"));
    let target = PathBuf::from(&path);
    if target.exists() {
        profile.cwd = Some(path.clone());
        spec.path = Some(path);
        spec.branch = Some(branch);
        spec.base_ref = Some(base_ref);
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("worker worktree mkdir error: {e}")))?;
    }
    let output = Command::new("git")
        .current_dir(&spec.repo)
        .args(["worktree", "add", "-B", &branch, &path, &base_ref])
        .output()
        .map_err(|e| VmError::Runtime(format!("worker worktree add failed: {e}")))?;
    if !output.status.success() {
        return Err(VmError::Runtime(format!(
            "worker worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    profile.cwd = Some(path.clone());
    spec.path = Some(path);
    spec.branch = Some(branch);
    spec.base_ref = Some(base_ref);
    Ok(())
}

fn cleanup_worker_execution(profile: &WorkerExecutionProfile) {
    let Some(spec) = &profile.worktree else {
        return;
    };
    if spec.cleanup.as_deref() != Some("remove") {
        return;
    }
    let Some(path) = spec.path.as_deref() else {
        return;
    };
    let _ = Command::new("git")
        .current_dir(&spec.repo)
        .args(["worktree", "remove", "--force", path])
        .output();
}

async fn execute_worker_config(
    worker_id: String,
    task: String,
    config: WorkerConfig,
    mut execution: WorkerExecutionProfile,
    audit: MutationSessionRecord,
) -> Result<WorkerExecutionResult, VmError> {
    ensure_worker_worktree(&worker_id, &mut execution)?;
    let execution_record = execution_record(&execution);
    crate::stdlib::process::set_thread_execution_context(Some(execution_record.clone()));
    crate::orchestration::install_current_mutation_session(Some(audit));
    let _mutation_guard = WorkerMutationSessionResetGuard;
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
            options.insert(
                "execution".to_string(),
                crate::stdlib::json_to_vm_value(
                    &serde_json::to_value(&execution_record).unwrap_or_default(),
                ),
            );
            options.insert("delegated".to_string(), VmValue::Bool(true));
            let result = super::workflow::execute_workflow(task, *graph, artifacts, options).await;
            crate::stdlib::process::set_thread_execution_context(None);
            cleanup_worker_execution(&execution);
            let result = result?;
            let dict = result.as_dict().ok_or_else(|| {
                VmError::Runtime("workflow execution returned a non-dict result".to_string())
            })?;
            let transcript = dict.get("transcript").cloned();
            let artifacts = parse_artifact_list(dict.get("artifacts"))?;
            Ok(WorkerExecutionResult {
                payload: crate::llm::vm_value_to_json(&VmValue::Dict(Rc::new(dict.clone()))),
                transcript,
                artifacts,
                execution,
            })
        }
        WorkerConfig::Stage {
            node,
            artifacts,
            transcript,
        } => {
            let _ = transcript;
            let result = crate::orchestration::execute_stage_node(
                "delegated_worker",
                &node,
                &task,
                &artifacts,
            )
            .await;
            crate::stdlib::process::set_thread_execution_context(None);
            cleanup_worker_execution(&execution);
            let (result, produced, next_transcript) = result?;
            Ok(WorkerExecutionResult {
                payload: serde_json::json!({
                    "status": "completed",
                    "mode": "stage",
                    "task": task,
                    "result": result,
                    "artifacts": produced,
                    "transcript": next_transcript.as_ref().map(crate::llm::vm_value_to_json),
                    "execution": execution_record,
                }),
                transcript: next_transcript,
                artifacts: produced,
                execution,
            })
        }
        WorkerConfig::SubAgent { spec } => {
            let result = super::execute_sub_agent(spec.as_ref().clone()).await?;
            Ok(WorkerExecutionResult {
                payload: result.payload,
                transcript: Some(result.transcript),
                artifacts: Vec::new(),
                execution,
            })
        }
    }
}

struct WorkerMutationSessionResetGuard;

impl Drop for WorkerMutationSessionResetGuard {
    fn drop(&mut self) {
        crate::orchestration::install_current_mutation_session(None);
    }
}

pub(super) fn spawn_worker_task(state: Rc<RefCell<WorkerState>>) {
    let (worker_id, task, config, execution, cancel_token, worker_policy, audit) = {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker).ok();
        }
        emit_worker_event(&worker, "running");
        (
            worker.id.clone(),
            worker.task.clone(),
            worker.config.clone(),
            worker.execution.clone(),
            worker.cancel_token.clone(),
            worker.carry_policy.policy.clone(),
            worker.audit.clone(),
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

        if let Some(ref policy) = worker_policy {
            push_execution_policy(policy.clone());
        }
        let worker_approval = audit.approval_policy.clone();
        if let Some(ref approval) = worker_approval {
            crate::orchestration::push_approval_policy(approval.clone());
        }
        let result = execute_worker_config(worker_id, task, config, execution, audit).await;
        if worker_approval.is_some() {
            crate::orchestration::pop_approval_policy();
        }
        if worker_policy.is_some() {
            pop_execution_policy();
        }
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
                    worker.execution = executed.execution.clone();
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
                    if let Some(run_id) = &worker.child_run_id {
                        worker.audit.run_id = Some(run_id.clone());
                    }
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
    state: &WorkerState,
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
        title: Some(format!("worker result {}", state.name)),
        text: if summary.is_empty() { None } else { Some(summary) },
        data: Some(serde_json::json!({
            "worker_id": state.id,
            "worker_name": state.name,
            "execution": state.execution,
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
            ("worker_id".to_string(), serde_json::json!(state.id)),
            ("worker_name".to_string(), serde_json::json!(state.name)),
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
    let execution = parse_execution_profile_json(node.metadata.get("execution"))?;
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
            resume_workflow: true,
            persist_state: true,
            policy: crate::orchestration::current_execution_policy(),
        },
        execution,
        snapshot_path: worker_snapshot_path(&worker_id),
        audit: MutationSessionRecord {
            parent_session_id: Some(node_id.to_string()),
            mutation_scope: "read_only".to_string(),
            approval_policy: crate::orchestration::current_approval_policy(),
            execution_kind: Some("delegated_stage".to_string()),
            ..Default::default()
        }
        .normalize(),
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
        &state.borrow(),
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
                node: Box::new(crate::orchestration::WorkflowNode {
                    kind: "stage".to_string(),
                    ..Default::default()
                }),
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
                resume_workflow: false,
                persist_state: true,
                policy: Some(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    side_effect_level: Some("read_only".to_string()),
                    ..Default::default()
                }),
            },
            execution: WorkerExecutionProfile::default(),
            snapshot_path: snapshot_path.clone(),
            audit: MutationSessionRecord {
                session_id: "session_worker_test".to_string(),
                parent_session_id: Some("session_parent".to_string()),
                run_id: Some("run_1".to_string()),
                worker_id: Some("worker_test".to_string()),
                execution_kind: Some("workflow".to_string()),
                mutation_scope: "apply_worktree".to_string(),
                approval_policy: None,
            }
            .normalize(),
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
            loaded.carry_policy.policy,
            Some(CapabilityPolicy {
                tools: vec!["read".to_string()],
                side_effect_level: Some("read_only".to_string()),
                ..Default::default()
            })
        );
        assert_eq!(loaded.audit.session_id, "session_worker_test");
        assert_eq!(loaded.audit.mutation_scope, "apply_worktree");

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

    #[test]
    fn worker_policy_inherits_parent_ceiling_when_unspecified() {
        crate::orchestration::push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            side_effect_level: Some("read_only".to_string()),
            ..Default::default()
        });

        let dict = BTreeMap::from([("task".to_string(), VmValue::String(Rc::from("draft note")))]);
        let resolved = resolve_worker_policy(&dict).unwrap();

        crate::orchestration::pop_execution_policy();

        assert_eq!(
            resolved,
            Some(CapabilityPolicy {
                tools: vec!["read".to_string()],
                side_effect_level: Some("read_only".to_string()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn worker_policy_intersects_explicit_policy_and_tools_shorthand() {
        crate::orchestration::push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string(), "write".to_string()],
            side_effect_level: Some("workspace_write".to_string()),
            ..Default::default()
        });

        let dict = BTreeMap::from([
            ("task".to_string(), VmValue::String(Rc::from("draft note"))),
            (
                "policy".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "tools".to_string(),
                    VmValue::List(Rc::new(vec![
                        VmValue::String(Rc::from("read")),
                        VmValue::String(Rc::from("write")),
                    ])),
                )]))),
            ),
            (
                "tools".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("read"))])),
            ),
        ]);
        let resolved = resolve_worker_policy(&dict).unwrap();

        crate::orchestration::pop_execution_policy();

        assert_eq!(
            resolved,
            Some(CapabilityPolicy {
                tools: vec!["read".to_string()],
                side_effect_level: Some("workspace_write".to_string()),
                ..Default::default()
            })
        );
    }
}
