use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};

use crate::stdlib::process::runtime_root_base;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_UPDATE_TIMEOUT_MS: u64 = 30_000;
const UPDATE_POLL_INTERVAL_MS: u64 = 25;

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
    let base = Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed);
    if base.is_empty() || base == "." || base == ".." {
        "workflow".to_string()
    } else {
        base.to_string()
    }
}

fn workflow_base_dir_from_persisted_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if parent.file_name().and_then(|value| value.to_str()) == Some(".harn-runs") {
        parent.parent().unwrap_or(parent).to_path_buf()
    } else {
        parent.to_path_buf()
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

fn enqueue_message(
    target: &WorkflowTarget,
    kind: &str,
    name: &str,
    payload: serde_json::Value,
    request_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut state = load_state(target)?;
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
    save_state(target, &state)?;
    Ok(serde_json::json!({
        "workflow_id": target.workflow_id,
        "message": message,
        "status": workflow_status_json(target, &state),
    }))
}

pub fn workflow_signal_for_base(
    base_dir: &Path,
    workflow_id: &str,
    name: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
    enqueue_message(&target, "signal", name, payload, None)
}

pub fn workflow_query_for_base(
    base_dir: &Path,
    workflow_id: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
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
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
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
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
    let mut state = load_state(&target)?;
    state.paused = true;
    state.next_seq += 1;
    state.mailbox.push_back(WorkflowMessageRecord {
        seq: state.next_seq,
        kind: "control".to_string(),
        name: "pause".to_string(),
        request_id: None,
        payload: serde_json::json!({}),
        enqueued_at: now_rfc3339(),
    });
    save_state(&target, &state)?;
    Ok(workflow_status_json(&target, &state))
}

pub fn workflow_resume_for_base(
    base_dir: &Path,
    workflow_id: &str,
) -> Result<serde_json::Value, String> {
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
    let mut state = load_state(&target)?;
    state.paused = false;
    state.next_seq += 1;
    state.mailbox.push_back(WorkflowMessageRecord {
        seq: state.next_seq,
        kind: "control".to_string(),
        name: "resume".to_string(),
        request_id: None,
        payload: serde_json::json!({}),
        enqueued_at: now_rfc3339(),
    });
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
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
    let request_id = uuid::Uuid::now_v7().to_string();
    enqueue_message(&target, "update", name, payload, Some(request_id.clone()))?;
    let started = std::time::Instant::now();
    while started.elapsed() <= timeout {
        if let Ok(state) = load_state(&target) {
            if let Some(response) = state.responses.get(&request_id) {
                return Ok(response.value.clone());
            }
        }
        tokio::time::sleep(StdDuration::from_millis(UPDATE_POLL_INTERVAL_MS)).await;
    }
    Err(format!(
        "workflow update '{name}' timed out for '{}'",
        target.workflow_id
    ))
}

pub fn workflow_respond_update_for_base(
    base_dir: &Path,
    workflow_id: &str,
    request_id: &str,
    name: Option<&str>,
    value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let target = WorkflowTarget {
        workflow_id: sanitize_workflow_id(workflow_id),
        base_dir: base_dir.to_path_buf(),
    };
    let mut state = load_state(&target)?;
    state.responses.insert(
        request_id.to_string(),
        WorkflowUpdateResponseRecord {
            request_id: request_id.to_string(),
            name: name.map(ToString::to_string),
            value,
            responded_at: now_rfc3339(),
        },
    );
    save_state(&target, &state)?;
    Ok(workflow_status_json(&target, &state))
}

pub(crate) fn register_workflow_message_builtins(vm: &mut Vm) {
    vm.set_global(
        "workflow",
        VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "signal".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.signal")),
            ),
            (
                "query".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.query")),
            ),
            (
                "update".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.update")),
            ),
            (
                "publish_query".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.publish_query")),
            ),
            (
                "receive".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.receive")),
            ),
            (
                "respond_update".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.respond_update")),
            ),
            (
                "pause".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.pause")),
            ),
            (
                "resume".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.resume")),
            ),
            (
                "status".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.status")),
            ),
            (
                "continue_as_new".to_string(),
                VmValue::BuiltinRef(Rc::from("workflow.continue_as_new")),
            ),
        ]))),
    );

    vm.register_builtin("workflow.signal", |args, _out| {
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
    });

    vm.register_builtin("workflow.query", |args, _out| {
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
    });

    vm.register_async_builtin("workflow.update", |args| async move {
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
    });

    vm.register_builtin("workflow.publish_query", |args, _out| {
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
    });

    vm.register_builtin("workflow.receive", |args, _out| {
        let target = parse_target_vm(args.first(), None, "workflow.receive")?;
        let mut state = load_state(&target).map_err(VmError::Runtime)?;
        let Some(message) = state.mailbox.pop_front() else {
            return Ok(VmValue::Nil);
        };
        save_state(&target, &state).map_err(VmError::Runtime)?;
        Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
            "workflow_id": target.workflow_id,
            "seq": message.seq,
            "kind": message.kind,
            "name": message.name,
            "request_id": message.request_id,
            "payload": message.payload,
            "enqueued_at": message.enqueued_at,
        })))
    });

    vm.register_builtin("workflow.respond_update", |args, _out| {
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
    });

    vm.register_builtin("workflow.pause", |args, _out| {
        let target = parse_target_vm(args.first(), None, "workflow.pause")?;
        let result = workflow_pause_for_base(&target.base_dir, &target.workflow_id)
            .map_err(VmError::Runtime)?;
        Ok(crate::stdlib::json_to_vm_value(&result))
    });

    vm.register_builtin("workflow.resume", |args, _out| {
        let target = parse_target_vm(args.first(), None, "workflow.resume")?;
        let result = workflow_resume_for_base(&target.base_dir, &target.workflow_id)
            .map_err(VmError::Runtime)?;
        Ok(crate::stdlib::json_to_vm_value(&result))
    });

    vm.register_builtin("workflow.status", |args, _out| {
        let target = parse_target_vm(args.first(), None, "workflow.status")?;
        let state = load_state(&target).map_err(VmError::Runtime)?;
        Ok(crate::stdlib::json_to_vm_value(&workflow_status_json(
            &target, &state,
        )))
    });

    vm.register_builtin("workflow.continue_as_new", |args, _out| {
        let target = parse_target_vm(args.first(), None, "workflow.continue_as_new")?;
        let mut state = load_state(&target).map_err(VmError::Runtime)?;
        state.generation += 1;
        state.continue_as_new_count += 1;
        state.last_continue_as_new_at = Some(now_rfc3339());
        state.responses.clear();
        save_state(&target, &state).map_err(VmError::Runtime)?;
        Ok(crate::stdlib::json_to_vm_value(&workflow_status_json(
            &target, &state,
        )))
    });

    vm.register_builtin("continue_as_new", |args, _out| {
        let target = parse_target_vm(args.first(), None, "continue_as_new")?;
        let mut state = load_state(&target).map_err(VmError::Runtime)?;
        state.generation += 1;
        state.continue_as_new_count += 1;
        state.last_continue_as_new_at = Some(now_rfc3339());
        state.responses.clear();
        save_state(&target, &state).map_err(VmError::Runtime)?;
        Ok(crate::stdlib::json_to_vm_value(&workflow_status_json(
            &target, &state,
        )))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_round_trip_waits_for_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_id = "wf-update";
        let base_dir = dir.path().to_path_buf();
        let base_dir_clone = base_dir.clone();
        let task = tokio::spawn(async move {
            workflow_update_for_base(
                &base_dir_clone,
                workflow_id,
                "adjust_budget",
                serde_json::json!({"max_usd": 10}),
                StdDuration::from_millis(500),
            )
            .await
        });

        tokio::time::sleep(StdDuration::from_millis(50)).await;
        let target = WorkflowTarget {
            workflow_id: workflow_id.to_string(),
            base_dir: base_dir.clone(),
        };
        let mut state = load_state(&target).expect("load state");
        let message = state.mailbox.pop_front().expect("queued update");
        assert_eq!(message.kind, "update");
        state.responses.insert(
            message.request_id.clone().expect("request id"),
            WorkflowUpdateResponseRecord {
                request_id: message.request_id.expect("request id"),
                name: Some(message.name.clone()),
                value: serde_json::json!({"ok": true}),
                responded_at: now_rfc3339(),
            },
        );
        save_state(&target, &state).expect("save response");

        let result = task.await.expect("join").expect("update result");
        assert_eq!(result, serde_json::json!({"ok": true}));
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
}
