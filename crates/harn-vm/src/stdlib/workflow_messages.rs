use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::process::runtime_root_base;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_UPDATE_TIMEOUT_MS: u64 = 30_000;
const UPDATE_POLL_INTERVAL_MS: u64 = 25;

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &WORKFLOW_SIGNAL_BUILTIN_DEF,
    &WORKFLOW_QUERY_BUILTIN_DEF,
    &WORKFLOW_PUBLISH_QUERY_BUILTIN_DEF,
    &WORKFLOW_RECEIVE_BUILTIN_DEF,
    &WORKFLOW_RESPOND_UPDATE_BUILTIN_DEF,
    &WORKFLOW_PAUSE_BUILTIN_DEF,
    &WORKFLOW_RESUME_BUILTIN_DEF,
    &WORKFLOW_STATUS_BUILTIN_DEF,
    &WORKFLOW_CONTINUE_AS_NEW_BUILTIN_DEF,
    &CONTINUE_AS_NEW_BUILTIN_DEF,
    &WORKFLOW_UPDATE_BUILTIN_DEF,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowMessageRecord {
    pub seq: u64,
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub payload: serde_json::Value,
    pub enqueued_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowQueryRecord {
    pub value: serde_json::Value,
    pub published_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowUpdateResponseRecord {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub value: serde_json::Value,
    pub responded_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowMailboxState {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub workflow_id: String,
    #[serde(default = "default_generation")]
    pub generation: u64,
    #[serde(default)]
    pub continue_as_new_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_continue_as_new_at: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub next_seq: u64,
    #[serde(default)]
    pub mailbox: VecDeque<WorkflowMessageRecord>,
    #[serde(default)]
    pub queries: BTreeMap<String, WorkflowQueryRecord>,
    #[serde(default)]
    pub responses: BTreeMap<String, WorkflowUpdateResponseRecord>,
}

impl Default for WorkflowMailboxState {
    fn default() -> Self {
        Self {
            type_name: "workflow_mailbox".to_string(),
            workflow_id: String::new(),
            generation: default_generation(),
            continue_as_new_count: 0,
            last_continue_as_new_at: None,
            paused: false,
            next_seq: 0,
            mailbox: VecDeque::new(),
            queries: BTreeMap::new(),
            responses: BTreeMap::new(),
        }
    }
}

fn default_generation() -> u64 {
    1
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkflowTarget {
    workflow_id: String,
    base_dir: PathBuf,
}

fn sanitize_workflow_id(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut sanitized = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "workflow".to_string()
    } else {
        sanitized
    }
}

fn workflow_base_dir_from_persisted_path(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if ancestor.file_name().and_then(|value| value.to_str()) == Some(".harn-runs") {
            return non_empty_path_or_dot(ancestor.parent().unwrap_or_else(|| Path::new(".")));
        }
    }
    non_empty_path_or_dot(path.parent().unwrap_or_else(|| Path::new(".")))
}

fn non_empty_path_or_dot(path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_path_buf()
    }
}

fn workflow_target_root(target: &WorkflowTarget) -> PathBuf {
    crate::runtime_paths::workflow_dir(&target.base_dir).join(&target.workflow_id)
}

fn workflow_state_path(target: &WorkflowTarget) -> PathBuf {
    workflow_target_root(target).join("state.json")
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| uuid::Uuid::now_v7().to_string())
}

fn load_state(target: &WorkflowTarget) -> Result<WorkflowMailboxState, String> {
    let path = workflow_state_path(target);
    if !path.exists() {
        return Ok(WorkflowMailboxState {
            workflow_id: target.workflow_id.clone(),
            ..WorkflowMailboxState::default()
        });
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("workflow state read error: {error}"))?;
    let mut state: WorkflowMailboxState = serde_json::from_str(&text)
        .map_err(|error| format!("workflow state parse error: {error}"))?;
    if state.type_name.is_empty() {
        state.type_name = "workflow_mailbox".to_string();
    }
    if state.workflow_id.is_empty() {
        state.workflow_id = target.workflow_id.clone();
    }
    if state.generation == 0 {
        state.generation = 1;
    }
    Ok(state)
}

fn save_state(target: &WorkflowTarget, state: &WorkflowMailboxState) -> Result<(), String> {
    let path = workflow_state_path(target);
    let json = serde_json::to_string_pretty(state)
        .map_err(|error| format!("workflow state encode error: {error}"))?;
    crate::atomic_io::atomic_write(&path, json.as_bytes())
        .map_err(|error| format!("workflow state write error: {error}"))
}

fn parse_target_json(
    value: &serde_json::Value,
    fallback_base_dir: Option<&Path>,
) -> Option<WorkflowTarget> {
    match value {
        serde_json::Value::String(text) => Some(WorkflowTarget {
            workflow_id: sanitize_workflow_id(text),
            base_dir: fallback_base_dir
                .map(Path::to_path_buf)
                .unwrap_or_else(runtime_root_base),
        }),
        serde_json::Value::Object(map) => {
            let workflow_id = map
                .get("workflow_id")
                .and_then(|value| value.as_str())
                .or_else(|| map.get("workflow").and_then(|value| value.as_str()))
                .or_else(|| {
                    map.get("run")
                        .and_then(|value| value.get("workflow_id"))
                        .and_then(|value| value.as_str())
                })
                .or_else(|| {
                    map.get("result")
                        .and_then(|value| value.get("run"))
                        .and_then(|value| value.get("workflow_id"))
                        .and_then(|value| value.as_str())
                })?;
            let explicit_base = map
                .get("base_dir")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from);
            let persisted_path = map
                .get("persisted_path")
                .and_then(|value| value.as_str())
                .or_else(|| map.get("path").and_then(|value| value.as_str()))
                .or_else(|| {
                    map.get("run")
                        .and_then(|value| value.get("persisted_path"))
                        .and_then(|value| value.as_str())
                })
                .or_else(|| {
                    map.get("result")
                        .and_then(|value| value.get("run"))
                        .and_then(|value| value.get("persisted_path"))
                        .and_then(|value| value.as_str())
                });
            let base_dir = explicit_base
                .or_else(|| {
                    persisted_path
                        .map(|path| workflow_base_dir_from_persisted_path(Path::new(path)))
                })
                .or_else(|| fallback_base_dir.map(Path::to_path_buf))
                .unwrap_or_else(runtime_root_base);
            Some(WorkflowTarget {
                workflow_id: sanitize_workflow_id(workflow_id),
                base_dir,
            })
        }
        _ => None,
    }
}

fn parse_target_vm(
    value: Option<&VmValue>,
    fallback_base_dir: Option<&Path>,
    builtin: &str,
) -> Result<WorkflowTarget, VmError> {
    let value = value.ok_or_else(|| VmError::Runtime(format!("{builtin}: missing target")))?;
    parse_target_json(&crate::llm::vm_value_to_json(value), fallback_base_dir).ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: target must be a workflow id string or dict with workflow_id/workflow"
        ))
    })
}

fn workflow_status_json(
    target: &WorkflowTarget,
    state: &WorkflowMailboxState,
) -> serde_json::Value {
    serde_json::json!({
        "workflow_id": target.workflow_id,
        "base_dir": target.base_dir.to_string_lossy(),
        "generation": state.generation,
        "paused": state.paused,
        "pending_count": state.mailbox.len(),
        "query_count": state.queries.len(),
        "response_count": state.responses.len(),
        "continue_as_new_count": state.continue_as_new_count,
        "last_continue_as_new_at": state.last_continue_as_new_at,
    })
}

fn target_for_base(base_dir: &Path, workflow_id: &str) -> WorkflowTarget {
    WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    }
}

fn enqueue_message(
    target: &WorkflowTarget,
    kind: &str,
    name: &str,
    payload: serde_json::Value,
    request_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut state = load_state(target)?;
    let message = push_message(&mut state, kind, name, payload, request_id);
    save_state(target, &state)?;
    Ok(serde_json::json!({
        "workflow_id": target.workflow_id,
        "message": message,
        "status": workflow_status_json(target, &state),
    }))
}

fn push_message(
    state: &mut WorkflowMailboxState,
    kind: &str,
    name: &str,
    payload: serde_json::Value,
    request_id: Option<String>,
) -> WorkflowMessageRecord {
    state.next_seq += 1;
    let message = WorkflowMessageRecord {
        seq: state.next_seq,
        kind: kind.to_string(),
        name: name.to_string(),
        request_id,
        payload,
        enqueued_at: now_rfc3339(),
    };
    state.mailbox.push_back(message.clone());
    message
}

fn receive_message(target: &WorkflowTarget) -> Result<Option<WorkflowMessageRecord>, String> {
    let mut state = load_state(target)?;
    let message = state.mailbox.pop_front();
    if message.is_some() {
        save_state(target, &state)?;
    }
    Ok(message)
}

pub fn workflow_signal_for_base(
    base_dir: &Path,
    workflow_id: &str,
    name: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    enqueue_message(&target, "signal", name, payload, None)
}

pub fn workflow_query_for_base(
    base_dir: &Path,
    workflow_id: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    let state = load_state(&target)?;
    Ok(state
        .queries
        .get(name)
        .map(|record| record.value.clone())
        .unwrap_or(serde_json::Value::Null))
}

pub fn workflow_publish_query_for_base(
    base_dir: &Path,
    workflow_id: &str,
    name: &str,
    value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    let mut state = load_state(&target)?;
    state.queries.insert(
        name.to_string(),
        WorkflowQueryRecord {
            value,
            published_at: now_rfc3339(),
        },
    );
    save_state(&target, &state)?;
    Ok(workflow_status_json(&target, &state))
}

pub fn workflow_pause_for_base(
    base_dir: &Path,
    workflow_id: &str,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    let mut state = load_state(&target)?;
    state.paused = true;
    push_message(&mut state, "control", "pause", serde_json::json!({}), None);
    save_state(&target, &state)?;
    Ok(workflow_status_json(&target, &state))
}

pub fn workflow_resume_for_base(
    base_dir: &Path,
    workflow_id: &str,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    let mut state = load_state(&target)?;
    state.paused = false;
    push_message(&mut state, "control", "resume", serde_json::json!({}), None);
    save_state(&target, &state)?;
    Ok(workflow_status_json(&target, &state))
}

pub async fn workflow_update_for_base(
    base_dir: &Path,
    workflow_id: &str,
    name: &str,
    payload: serde_json::Value,
    timeout: StdDuration,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    let request_id = enqueue_update_request(&target, name, payload)?;
    wait_for_update_response(&target, name, &request_id, timeout).await
}

fn enqueue_update_request(
    target: &WorkflowTarget,
    name: &str,
    payload: serde_json::Value,
) -> Result<String, String> {
    let request_id = uuid::Uuid::now_v7().to_string();
    enqueue_message(target, "update", name, payload, Some(request_id.clone()))?;
    Ok(request_id)
}

async fn wait_for_update_response(
    target: &WorkflowTarget,
    name: &str,
    request_id: &str,
    timeout: StdDuration,
) -> Result<serde_json::Value, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match update_response_value(target, request_id) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(error) => return Err(error),
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let next_poll = now + StdDuration::from_millis(UPDATE_POLL_INTERVAL_MS);
        tokio::time::sleep_until(next_poll.min(deadline)).await;
    }
    Err(format!(
        "workflow update '{name}' timed out for '{}'",
        target.workflow_id
    ))
}

fn update_response_value(
    target: &WorkflowTarget,
    request_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    Ok(load_state(target)?
        .responses
        .get(request_id)
        .map(|response| response.value.clone()))
}

pub fn workflow_respond_update_for_base(
    base_dir: &Path,
    workflow_id: &str,
    request_id: &str,
    name: Option<&str>,
    value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let target = target_for_base(base_dir, workflow_id);
    record_update_response(&target, request_id, name, value)
}

fn record_update_response(
    target: &WorkflowTarget,
    request_id: &str,
    name: Option<&str>,
    value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut state = load_state(target)?;
    state.responses.insert(
        request_id.to_string(),
        WorkflowUpdateResponseRecord {
            request_id: request_id.to_string(),
            name: name.map(ToString::to_string),
            value,
            responded_at: now_rfc3339(),
        },
    );
    save_state(target, &state)?;
    Ok(workflow_status_json(target, &state))
}

pub(crate) fn register_workflow_message_builtins(vm: &mut Vm) {
    vm.set_global(
        "workflow",
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "signal".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.signal")),
            ),
            (
                "query".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.query")),
            ),
            (
                "update".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.update")),
            ),
            (
                "publish_query".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.publish_query")),
            ),
            (
                "receive".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.receive")),
            ),
            (
                "respond_update".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.respond_update")),
            ),
            (
                "pause".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.pause")),
            ),
            (
                "resume".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.resume")),
            ),
            (
                "status".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.status")),
            ),
            (
                "continue_as_new".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("workflow.continue_as_new")),
            ),
        ]))),
    );

    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

/// Enqueue a workflow signal message.
#[harn_builtin(
    sig = "workflow.signal(target: string|dict, name: string, payload?: any) -> dict",
    category = "workflow.messages"
)]
fn workflow_signal_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.signal")?;
    let name = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("workflow.signal: missing name".to_string()))?;
    let payload = args
        .get(2)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let result =
        enqueue_message(&target, "signal", &name, payload, None).map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

/// Read the latest published workflow query value.
#[harn_builtin(
    sig = "workflow.query(target: string|dict, name: string) -> any",
    category = "workflow.messages"
)]
fn workflow_query_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.query")?;
    let name = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("workflow.query: missing name".to_string()))?;
    let state = load_state(&target).map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(
        &state
            .queries
            .get(&name)
            .map(|record| record.value.clone())
            .unwrap_or(serde_json::Value::Null),
    ))
}

/// Enqueue a workflow update and wait for a response.
#[harn_builtin(
    sig = "workflow.update(target: string|dict, name: string, payload?: any, options?: dict|nil) -> any",
    kind = "async",
    category = "workflow.messages"
)]
async fn workflow_update_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.update")?;
    let name = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("workflow.update: missing name".to_string()))?;
    let payload = args
        .get(2)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let timeout_ms = args
        .get(3)
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("timeout_ms"))
        .and_then(VmValue::as_int)
        .unwrap_or(DEFAULT_UPDATE_TIMEOUT_MS as i64)
        .max(1) as u64;
    let result = workflow_update_for_base(
        &target.base_dir,
        &target.workflow_id,
        &name,
        payload,
        StdDuration::from_millis(timeout_ms),
    )
    .await
    .map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

/// Publish a workflow query value.
#[harn_builtin(
    sig = "workflow.publish_query(target: string|dict, name: string, value?: any) -> dict",
    category = "workflow.messages"
)]
fn workflow_publish_query_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.publish_query")?;
    let name = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("workflow.publish_query: missing name".to_string()))?;
    let value = args
        .get(2)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let result =
        workflow_publish_query_for_base(&target.base_dir, &target.workflow_id, &name, value)
            .map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

/// Receive the next workflow mailbox message.
#[harn_builtin(
    sig = "workflow.receive(target: string|dict) -> dict|nil",
    category = "workflow.messages"
)]
fn workflow_receive_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.receive")?;
    let Some(message) = receive_message(&target).map_err(VmError::Runtime)? else {
        return Ok(VmValue::Nil);
    };
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "workflow_id": target.workflow_id,
        "seq": message.seq,
        "kind": message.kind,
        "name": message.name,
        "request_id": message.request_id,
        "payload": message.payload,
        "enqueued_at": message.enqueued_at,
    })))
}

/// Respond to a pending workflow update request.
#[harn_builtin(
    sig = "workflow.respond_update(target: string|dict, request_id: string, value?: any, name?: string|nil) -> dict",
    category = "workflow.messages"
)]
fn workflow_respond_update_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.respond_update")?;
    let request_id = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("workflow.respond_update: missing request id".to_string())
        })?;
    let value = args
        .get(2)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let name = args
        .get(3)
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    let result = workflow_respond_update_for_base(
        &target.base_dir,
        &target.workflow_id,
        &request_id,
        name.as_deref(),
        value,
    )
    .map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

/// Pause a workflow mailbox.
#[harn_builtin(
    sig = "workflow.pause(target: string|dict) -> dict",
    category = "workflow.messages"
)]
fn workflow_pause_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.pause")?;
    let result =
        workflow_pause_for_base(&target.base_dir, &target.workflow_id).map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

/// Resume a workflow mailbox.
#[harn_builtin(
    sig = "workflow.resume(target: string|dict) -> dict",
    category = "workflow.messages"
)]
fn workflow_resume_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.resume")?;
    let result = workflow_resume_for_base(&target.base_dir, &target.workflow_id)
        .map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

/// Return workflow mailbox status.
#[harn_builtin(
    sig = "workflow.status(target: string|dict) -> dict",
    category = "workflow.messages"
)]
fn workflow_status_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, "workflow.status")?;
    let state = load_state(&target).map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&workflow_status_json(
        &target, &state,
    )))
}

/// Advance a workflow mailbox generation.
#[harn_builtin(
    sig = "workflow.continue_as_new(target: string|dict) -> dict",
    category = "workflow.messages"
)]
fn workflow_continue_as_new_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    continue_as_new_for_label(args, "workflow.continue_as_new")
}

/// Advance a workflow mailbox generation (top-level alias).
#[harn_builtin(
    sig = "continue_as_new(target: string|dict) -> dict",
    category = "workflow.messages"
)]
fn continue_as_new_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    continue_as_new_for_label(args, "continue_as_new")
}

fn continue_as_new_for_label(args: &[VmValue], label: &str) -> Result<VmValue, VmError> {
    let target = parse_target_vm(args.first(), None, label)?;
    let mut state = load_state(&target).map_err(VmError::Runtime)?;
    state.generation += 1;
    state.continue_as_new_count += 1;
    state.last_continue_as_new_at = Some(now_rfc3339());
    state.responses.clear();
    save_state(&target, &state).map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&workflow_status_json(
        &target, &state,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    #[tokio::test(start_paused = true)]
    async fn update_round_trip_waits_for_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_id = "wf-update";
        let base_dir = dir.path().to_path_buf();
        let target = target_for_base(&base_dir, workflow_id);
        let request_id =
            enqueue_update_request(&target, "adjust_budget", serde_json::json!({"max_usd": 10}))
                .expect("enqueue update");

        let message = receive_message(&target)
            .expect("receive queued update")
            .expect("queued update");
        assert_eq!(message.kind, "update");
        assert_eq!(message.name, "adjust_budget");
        assert_eq!(message.request_id.as_deref(), Some(request_id.as_str()));
        assert_eq!(
            update_response_value(&target, &request_id).expect("read response"),
            None
        );

        let waiter = wait_for_update_response(
            &target,
            "adjust_budget",
            &request_id,
            StdDuration::from_millis(500),
        );
        tokio::pin!(waiter);
        assert!(matches!(futures::poll!(&mut waiter), Poll::Pending));

        workflow_respond_update_for_base(
            &base_dir,
            workflow_id,
            &request_id,
            Some("adjust_budget"),
            serde_json::json!({"ok": true}),
        )
        .expect("save response");
        assert_eq!(
            update_response_value(&target, &request_id).expect("read response"),
            Some(serde_json::json!({"ok": true}))
        );
        tokio::time::advance(StdDuration::from_millis(UPDATE_POLL_INTERVAL_MS)).await;

        let result = waiter.await.expect("update result");
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test(start_paused = true)]
    async fn update_wait_respects_short_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = target_for_base(dir.path(), "wf-timeout");
        let request_id =
            enqueue_update_request(&target, "adjust_budget", serde_json::json!({"max_usd": 10}))
                .expect("enqueue update");

        let waiter = wait_for_update_response(
            &target,
            "adjust_budget",
            &request_id,
            StdDuration::from_millis(10),
        );
        tokio::pin!(waiter);
        assert!(matches!(futures::poll!(&mut waiter), Poll::Pending));

        tokio::time::advance(StdDuration::from_millis(9)).await;
        assert!(matches!(futures::poll!(&mut waiter), Poll::Pending));

        tokio::time::advance(StdDuration::from_millis(1)).await;
        let err = waiter.await.expect_err("update should time out");
        assert!(err.contains("timed out"));
    }

    #[tokio::test]
    async fn update_wait_propagates_state_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = target_for_base(dir.path(), "wf-corrupt");
        std::fs::create_dir_all(workflow_target_root(&target)).expect("state dir");
        std::fs::write(workflow_state_path(&target), "{not json").expect("state write");

        let err = wait_for_update_response(
            &target,
            "adjust_budget",
            "request-1",
            StdDuration::from_millis(10),
        )
        .await
        .expect_err("corrupt state should fail immediately");
        assert!(err.contains("workflow state parse error"));
    }

    #[test]
    fn workflow_ids_preserve_namespace_without_path_segments() {
        assert_eq!(
            sanitize_workflow_id("workflow://local/start-my-day"),
            "workflow___local_start-my-day"
        );
        assert_eq!(sanitize_workflow_id("../start-my-day"), ".._start-my-day");
        assert_eq!(sanitize_workflow_id(".."), "workflow");
    }

    #[test]
    fn persisted_path_drives_target_base_dir() {
        let base = parse_target_json(
            &serde_json::json!({
                "workflow_id": "wf",
                "persisted_path": "/tmp/demo/.harn-runs/run.json"
            }),
            None,
        )
        .expect("target");
        assert_eq!(base.workflow_id, "wf");
        assert_eq!(base.base_dir, PathBuf::from("/tmp/demo"));
    }

    #[test]
    fn nested_persisted_path_drives_target_base_dir() {
        let base = parse_target_json(
            &serde_json::json!({
                "workflow_id": "wf",
                "persisted_path": "/tmp/demo/.harn-runs/session/run.json"
            }),
            None,
        )
        .expect("target");
        assert_eq!(base.base_dir, PathBuf::from("/tmp/demo"));

        let relative = parse_target_json(
            &serde_json::json!({
                "workflow_id": "wf",
                "persisted_path": ".harn-runs/session/run.json"
            }),
            None,
        )
        .expect("target");
        assert_eq!(relative.base_dir, PathBuf::from("."));
    }
}
