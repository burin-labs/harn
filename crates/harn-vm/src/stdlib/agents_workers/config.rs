use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::super::{parse_artifact_list, parse_context_policy, SubAgentRunSpec};
use super::audit::parse_worker_audit;
use super::bridge::worker_snapshot_path;
use super::policy::{
    parse_transcript_mode, parse_worker_carry_policy, parse_worker_policy_value,
    resolve_worker_policy, worker_policy_value,
};
use super::{
    WorkerCarryPolicy, WorkerConfig, WorkerExecutionProfile, WorkerInit, WorkerRequestRecord,
    WorkerState,
};
use crate::orchestration::{ArtifactRecord, MutationSessionRecord, WorkflowGraph};
use crate::stdlib::options::{ErrorKind, OptionsParser};
use crate::value::{VmError, VmValue};

const SPAWN_AGENT_FN: &str = "spawn_agent";
const WORKER_SNAPSHOT_CONFIG: &str = "worker snapshot config";

fn worker_config_to_json(config: &WorkerConfig) -> Result<serde_json::Value, VmError> {
    match config {
        WorkerConfig::Workflow {
            graph,
            artifacts,
            options,
        } => {
            // Workflow options are user-provided data destined for
            // `std/workflow/execute`, which already round-trips them
            // through JSON on the resume path — a closure or handle in
            // here was never going to survive persistence. Fail loud at
            // save time (with the offending path) instead of writing a
            // display-string that rehydrates as a plain string and
            // explodes as a type error long after resume.
            let options =
                crate::llm::helpers::vm_value_dict_to_json_strict(options, "options")
                    .map_err(|error| {
                        VmError::Runtime(format!("worker snapshot: {error}"))
                    })?;
            Ok(serde_json::json!({
                "mode": "workflow",
                "graph": graph,
                "artifacts": artifacts,
                "options": options,
            }))
        }
        WorkerConfig::Stage {
            node,
            artifacts,
            transcript,
        } => Ok(serde_json::json!({
            "mode": "stage",
            "node": node,
            "artifacts": artifacts,
            "transcript": transcript.as_ref().map(crate::llm::vm_value_to_json),
        })),
        WorkerConfig::SubAgent { spec } => Ok(serde_json::json!({
            "mode": "sub_agent",
            "spec": sub_agent_spec_to_json(spec)?,
        })),
    }
}

fn sub_agent_spec_to_json(spec: &SubAgentRunSpec) -> Result<serde_json::Value, VmError> {
    // Sub-agent specs are persisted from *live* agent-loop options: the
    // top-level suspend path (`__host_top_level_agent_suspend`) hands the
    // in-flight `opts` dict over wholesale, and those legitimately carry
    // closures (`tool_caller` / `_tool_caller` middleware, tool handlers,
    // custom compactors). Hard-erroring here would break every top-level
    // suspend that uses callbacks, so instead we strip the non-serializable
    // entries at save time and emit a WARN naming each dropped path. The
    // callbacks were never going to survive the process exit anyway; the
    // resume path re-binds them from the live agent-loop configuration.
    let mut options = serde_json::Map::new();
    for (key, value) in spec.options.iter() {
        match crate::llm::vm_value_to_json_strict(value, &format!("spec.options.{key}")) {
            Ok(json) => {
                options.insert(key.to_string(), json);
            }
            Err(error) => {
                crate::events::emit_log(
                    crate::events::EventLevel::Warn,
                    "agents",
                    &format!(
                        "worker snapshot for sub-agent '{}': dropping non-serializable \
                         option ({error}); live callbacks cannot survive persistence and \
                         are re-bound from the agent-loop configuration on resume",
                        spec.name
                    ),
                    std::collections::BTreeMap::new(),
                );
            }
        }
    }
    // `returns_schema` is pure data (a schema literal); a closure in it is
    // a caller bug, so the strict error propagates instead of stripping.
    let returns_schema = spec
        .returns_schema
        .as_ref()
        .map(|schema| crate::llm::vm_value_to_json_strict(schema, "spec.returns_schema"))
        .transpose()
        .map_err(|error| VmError::Runtime(format!("worker snapshot: {error}")))?;
    Ok(serde_json::json!({
        "name": &spec.name,
        "task": &spec.task,
        "system": &spec.system,
        "options": options,
        "returns_schema": returns_schema,
        "session_id": &spec.session_id,
        "parent_session_id": &spec.parent_session_id,
        "reminder_propagation": &spec.reminder_propagation,
        "workspace_anchor": spec
            .workspace_anchor
            .as_ref()
            .map(crate::workspace_anchor::WorkspaceAnchor::to_json),
    }))
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
                .map(|(key, value)| {
                    (
                        crate::value::intern_key(key),
                        crate::stdlib::json_to_vm_value(value),
                    )
                })
                .collect::<crate::value::DictMap>()
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
        reminder_propagation: dict
            .get("reminder_propagation")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        serde_json::from_value(item.clone()).map_err(|error| {
                            VmError::Runtime(format!(
                                "worker snapshot sub-agent reminder_propagation parse error: {error}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default(),
        workspace_anchor: dict
            .get("workspace_anchor")
            .filter(|value| !value.is_null())
            .map(|value| {
                crate::workspace_anchor::WorkspaceAnchor::from_json(value).map_err(|error| {
                    VmError::Runtime(format!(
                        "worker snapshot sub-agent workspace_anchor parse error: {error}"
                    ))
                })
            })
            .transpose()?,
    })
}

fn worker_config_from_json(value: &serde_json::Value) -> Result<WorkerConfig, VmError> {
    let parser_dict = value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| {
                    (
                        crate::value::intern_key(key),
                        crate::stdlib::json_to_vm_value(value),
                    )
                })
                .collect::<crate::value::DictMap>()
        })
        .unwrap_or_default();
    let mut parser = OptionsParser::new(WORKER_SNAPSHOT_CONFIG, &parser_dict, ErrorKind::Runtime);
    let mode = parser.optional_string_raw("mode")?.unwrap_or_default();
    match mode.as_str() {
        "workflow" => {
            parser.allow("graph");
            parser.allow("artifacts");
            let graph: WorkflowGraph = serde_json::from_value(
                value.get("graph").cloned().unwrap_or_default(),
            )
            .map_err(|e| VmError::Runtime(format!("worker snapshot graph parse error: {e}")))?;
            let artifacts: Vec<ArtifactRecord> =
                serde_json::from_value(value.get("artifacts").cloned().unwrap_or_default())
                    .map_err(|e| {
                        VmError::Runtime(format!("worker snapshot artifacts parse error: {e}"))
                    })?;
            let options = parser
                .optional_dict("options")?
                .cloned()
                .unwrap_or_default();
            parser.finish_strict(&[])?;
            Ok(WorkerConfig::Workflow {
                graph: Box::new(graph),
                artifacts,
                options,
            })
        }
        "stage" => {
            parser.allow("node");
            parser.allow("artifacts");
            parser.allow("transcript");
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
            parser.finish_strict(&[])?;
            Ok(WorkerConfig::Stage {
                node: Box::new(node),
                artifacts,
                transcript,
            })
        }
        "sub_agent" => {
            parser.allow("spec");
            let spec =
                sub_agent_spec_from_json(value.get("spec").unwrap_or(&serde_json::Value::Null))
                    .map_err(|e| {
                        VmError::Runtime(format!("worker snapshot sub-agent parse error: {e}"))
                    })?;
            parser.finish_strict(&[])?;
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
    let mut payload = serde_json::json!({
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
        "config": worker_config_to_json(&state.config)?,
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
            "transcript_mode": state.carry_policy.transcript_mode,
            "context_policy": state.carry_policy.context_policy,
            "resume_workflow": state.carry_policy.resume_workflow,
            "persist_state": state.carry_policy.persist_state,
            "retriggerable": state.carry_policy.retriggerable,
            "policy": state.carry_policy.policy,
        },
        "execution": state.execution,
        "snapshot_path": state.snapshot_path,
        "audit": state.audit,
        "suspension": state.suspension,
    });
    // Worker snapshots land on disk verbatim, and both `options` and the
    // transcript routinely carry user-supplied headers/tokens. Scrub with
    // the same unified redaction policy every other persistence surface
    // (receipts, event logs, portal JSON) already uses so a secret cannot
    // leak through this file when it is redacted everywhere else.
    crate::redact::current_policy().redact_json_in_place(&mut payload);
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
        let transcript_mode = dict
            .get("transcript_mode")
            .map(parse_transcript_mode)
            .transpose()?
            .unwrap_or_else(|| "inherit".to_string());
        WorkerCarryPolicy {
            artifact_mode,
            transcript_mode,
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
    // `running` snapshots come from in-flight workers whose Tokio handle
    // didn't survive the process exit — they're rehydrated as
    // `interrupted` so the operator must explicitly re-arm them. A
    // `suspended` snapshot, by contrast, is a deliberate cooperative
    // checkpoint (harn#1836); cold-restoring it must land back in the
    // same suspended state so the resume primitive can warm it up.
    let normalized_status = if status == "running" {
        "interrupted".to_string()
    } else {
        status.to_string()
    };
    let suspension: Option<super::WorkerSuspension> =
        serde_json::from_value(payload.get("suspension").cloned().unwrap_or_default())
            .unwrap_or_default();
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
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension,
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
        Some(VmValue::Nil) => Ok(WorkerExecutionProfile::default()),
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
        .ok_or_else(|| VmError::Runtime(format!("{SPAWN_AGENT_FN}: config must be a dict")))?;
    let mut parser = OptionsParser::new(SPAWN_AGENT_FN, dict, ErrorKind::Runtime);
    let task = parser.required_string("task")?;
    let name = parser
        .optional_string_raw("name")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "worker".to_string());
    let wait = parser.bool_or("wait", false)?;
    parser.allow("carry");
    parser.allow("policy");
    parser.allow("tools");
    parser.allow("audit");
    let mut carry_policy = parse_worker_carry_policy(dict)?;
    carry_policy.policy = resolve_worker_policy(dict)?;
    let execution = parse_worker_execution_profile(parser.raw("execution"))?;
    let audit = parse_worker_audit(dict)?;
    let graph_value = parser.raw("graph");
    let node_value = parser.raw("node");
    let artifacts_value = parser.raw("artifacts");
    let permissions = parser.raw("permissions").cloned();
    let transcript = parser.raw("transcript").cloned();
    let options = parser
        .optional_dict("options")?
        .cloned()
        .unwrap_or_default();

    let config = if let Some(graph_value) = graph_value {
        let graph = crate::orchestration::normalize_workflow_value(graph_value)?;
        let artifacts = parse_artifact_list(artifacts_value)?;
        WorkerConfig::Workflow {
            graph: Box::new(graph),
            artifacts,
            options,
        }
    } else {
        let node_value = node_value.ok_or_else(|| {
            VmError::Runtime(format!(
                "{SPAWN_AGENT_FN}: config requires either graph or node"
            ))
        })?;
        let mut node =
            crate::orchestration::parse_workflow_node_value(node_value, "spawn_agent node")?;
        if let Some(permissions) = permissions {
            let mut raw_model_policy = node
                .raw_model_policy
                .as_ref()
                .and_then(|value| value.as_dict())
                .cloned()
                .unwrap_or_default();
            raw_model_policy.insert(crate::value::intern_key("permissions"), permissions);
            node.raw_model_policy = Some(VmValue::dict(raw_model_policy));
        }
        let artifacts = parse_artifact_list(artifacts_value)?;
        WorkerConfig::Stage {
            node: Box::new(node),
            artifacts,
            transcript,
        }
    };
    parser.finish_strict(&[])?;
    Ok(WorkerInit {
        name,
        task,
        config,
        wait,
        carry_policy,
        execution,
        audit,
    })
}
