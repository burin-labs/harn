//! Workflow run state machine and stage-lifecycle helpers.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::orchestration::{
    builtin_ceiling, install_current_mutation_session, install_workflow_skill_context,
    load_run_record, normalize_run_record, push_execution_policy, validate_workflow,
    workflow_verification_contracts, ArtifactRecord, MutationSessionRecord,
    PreparedWorkflowStageNode, RunRecord, RunStageRecord, RunTransitionRecord,
    VerificationContract, WorkflowEdge, WorkflowGraph, WorkflowSkillContext,
    WorkflowSkillContextGuard,
};
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::artifact::{
    append_child_run_record, checkpoint_run, optional_string_option, parse_execution_record,
    snapshot_trace_spans,
};
use super::convert::{to_vm, workflow_graph_to_vm};
use super::guards::{
    MutationSessionResetGuard, WorkflowApprovalPolicyGuard, WorkflowExecutionPolicyGuard,
};
use super::map::MapWorkItem;
use super::policy::{
    apply_runtime_node_overrides, effective_node_approval_policy, effective_node_policy,
};
use super::stage::{execute_stage_attempts, replay_stage, stage_attempt_outcome, ExecutedStage};
use super::usage::{llm_usage_delta, llm_usage_snapshot, UsageSnapshot};

thread_local! {
    pub(super) static WORKFLOW_RUN_STATES: RefCell<BTreeMap<String, WorkflowRunState>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

pub(super) struct WorkflowRunState {
    pub(super) state_id: String,
    pub(super) graph: WorkflowGraph,
    pub(super) run: RunRecord,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) transcript: Option<serde_json::Value>,
    pub(super) ready_nodes: Vec<String>,
    pub(super) completed_nodes: Vec<String>,
    pub(super) persist_path: String,
    pub(super) replay_mode: Option<String>,
    pub(super) replay_stages: Option<Vec<RunStageRecord>>,
    pub(super) workflow_verification_contracts: Vec<VerificationContract>,
    pub(super) mutation_session: MutationSessionRecord,
    pub(super) run_usage_before: UsageSnapshot,
    pub(super) workflow_span_id: u64,
    pub(super) max_steps: usize,
    pub(super) stage_scope: Option<StageExecutionScope>,
}

pub(super) struct StageExecutionScope {
    pub(super) node_id: String,
    pub(super) node: crate::orchestration::WorkflowNode,
    pub(super) stage_policy: crate::orchestration::CapabilityPolicy,
    pub(super) stage_id: String,
    pub(super) started_at: String,
    pub(super) execution: StageExecution,
    pub(super) _mutation_session_guard: MutationSessionResetGuard,
    pub(super) _workflow_skill_guard: WorkflowSkillContextGuard,
    pub(super) _workflow_approval_guard: WorkflowApprovalPolicyGuard,
    pub(super) _stage_execution_policy_guard: WorkflowExecutionPolicyGuard,
    pub(super) _stage_approval_guard: WorkflowApprovalPolicyGuard,
    pub(super) _runtime_context_guard: crate::runtime_context::RuntimeContextOverlayGuard,
}

pub(super) enum StageExecution {
    Precomputed(ExecutedStage),
    HarnCall {
        prepared: PreparedWorkflowStageNode,
        usage_before: UsageSnapshot,
        attempt_started_at: String,
        input_transcript: Option<VmValue>,
    },
    HarnMapCall {
        artifacts: Vec<ArtifactRecord>,
        map_options: crate::value::DictMap,
        usage_before: UsageSnapshot,
        attempt_started_at: String,
        input_transcript: Option<VmValue>,
        consumed_artifact_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct WorkflowExecutedStageRecord {
    pub(super) stage_id: String,
    pub(super) node_id: String,
    pub(super) branch: Option<String>,
    pub(super) consumed_artifact_ids: Vec<String>,
    pub(super) produced_artifact_ids: Vec<String>,
}

pub(super) fn parse_options_arg(args: &[VmValue], index: usize) -> crate::value::DictMap {
    args.get(index)
        .and_then(|v| v.as_dict())
        .cloned()
        .unwrap_or_default()
}

pub(super) fn string_list_to_vm(values: &[String]) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        values
            .iter()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value.as_str())))
            .collect(),
    ))
}

pub(super) fn parse_state_id_arg(
    value: Option<&VmValue>,
    context: &str,
) -> Result<String, VmError> {
    value
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{context}: missing workflow state id")))
}

pub(super) fn parse_string_list_arg(
    value: Option<&VmValue>,
    context: &str,
) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        VmValue::Nil => Ok(Vec::new()),
        VmValue::List(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| match value {
                VmValue::String(text) => Ok(text.to_string()),
                _ => Err(VmError::Runtime(format!(
                    "{context}: expected string at index {index}"
                ))),
            })
            .collect(),
        _ => Err(VmError::Runtime(format!("{context}: expected list"))),
    }
}

pub(super) fn parse_json_arg<T: serde::de::DeserializeOwned>(
    value: Option<&VmValue>,
    context: &str,
) -> Result<T, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value.unwrap_or(&VmValue::Nil)))
        .map_err(|error| VmError::Runtime(format!("{context}: invalid value: {error}")))
}

pub(super) fn map_item_index(item: &MapWorkItem) -> usize {
    match item {
        MapWorkItem::Artifact { index, .. } | MapWorkItem::Value { index, .. } => *index,
    }
}

pub(super) fn workflow_control_to_vm(
    state: &WorkflowRunState,
    include_graph: bool,
) -> Result<VmValue, VmError> {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("state_id", state.state_id.as_str());
    dict.put_str("run_id", state.run.id.as_str());
    dict.insert(
        crate::value::intern_key("ready_nodes"),
        string_list_to_vm(&state.ready_nodes),
    );
    dict.insert(
        crate::value::intern_key("completed_nodes"),
        string_list_to_vm(&state.completed_nodes),
    );
    dict.insert(
        crate::value::intern_key("max_steps"),
        VmValue::Int(state.max_steps as i64),
    );
    if include_graph {
        dict.insert(
            crate::value::intern_key("graph"),
            workflow_graph_to_vm(&state.graph)?,
        );
    }
    Ok(VmValue::dict(dict))
}

pub(super) fn insert_workflow_state(state: WorkflowRunState) {
    WORKFLOW_RUN_STATES.with(|states| {
        states.borrow_mut().insert(state.state_id.clone(), state);
    });
}

/// Drop every in-flight workflow run state. Normal completion removes a
/// state via [`remove_workflow_state`], but a workflow that is abandoned
/// mid-run (test timeout, early error, host teardown) leaves its
/// transcript, tool recordings, and artifacts interned here. The
/// per-test reset path calls this so a reused worker thread does not
/// accumulate one transcript-sized entry per workflow turn.
pub(crate) fn reset_workflow_run_states() {
    WORKFLOW_RUN_STATES.with(|states| states.borrow_mut().clear());
}

/// Number of interned workflow run states on this thread. Test-only.
#[cfg(test)]
pub(crate) fn workflow_run_state_count() -> usize {
    WORKFLOW_RUN_STATES.with(|states| states.borrow().len())
}

pub(super) fn remove_workflow_state(
    state_id: &str,
    context: &str,
) -> Result<WorkflowRunState, VmError> {
    WORKFLOW_RUN_STATES.with(|states| {
        states.borrow_mut().remove(state_id).ok_or_else(|| {
            VmError::Runtime(format!("{context}: unknown workflow state id {state_id}"))
        })
    })
}

pub(super) fn parse_executed_stage_record(
    value: &VmValue,
    context: &str,
) -> Result<WorkflowExecutedStageRecord, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map_err(|error| VmError::Runtime(format!("{context}: invalid stage record: {error}")))
}

fn parse_trigger_event_option(
    value: Option<&VmValue>,
) -> Result<Option<crate::TriggerEvent>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map(Some)
        .map_err(|error| {
            VmError::Runtime(format!(
                "workflow_execute: trigger_event parse error: {error}"
            ))
        })
}

/// Accept a skill registry dict on `workflow_execute(task, graph,
/// artifacts, {skills: ...})`. Mirrors the agent-loop normalizer: a
/// raw registry dict passes through, a list of skill entries is
/// wrapped into a synthetic registry. Other shapes are dropped —
/// failing silently rather than erroring keeps backwards compatibility
/// for callers that already pass unvalidated option dicts through.
fn validate_workflow_skill_registry(value: VmValue) -> Option<VmValue> {
    match &value {
        VmValue::Dict(d)
            if d.get("_type")
                .map(|v| v.display() == "skill_registry")
                .unwrap_or(false) =>
        {
            Some(value)
        }
        VmValue::List(list) => {
            let mut dict = crate::value::DictMap::new();
            dict.put_str("_type", "skill_registry");
            dict.insert(
                crate::value::intern_key("skills"),
                VmValue::List(list.clone()),
            );
            Some(VmValue::dict(dict))
        }
        _ => None,
    }
}

pub(super) fn checkpoint_workflow_state(
    state: &mut WorkflowRunState,
    last_stage_id: Option<String>,
    reason: &str,
) -> Result<(), VmError> {
    let ready_nodes = VecDeque::from(state.ready_nodes.clone());
    let completed_nodes = state
        .completed_nodes
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    checkpoint_run(
        &mut state.run,
        &ready_nodes,
        &completed_nodes,
        last_stage_id,
        reason,
        &state.persist_path,
    )
}

pub(super) fn prepare_workflow_state(
    task: String,
    graph: WorkflowGraph,
    mut artifacts: Vec<ArtifactRecord>,
    options: &crate::value::DictMap,
) -> Result<WorkflowRunState, VmError> {
    crate::llm::enable_tracing();
    // Preserve any enclosing trace: if the caller already holds an open
    // span (e.g. a run-level `start_timing` / `timed(...)` handle held
    // across this workflow run), a plain `set_tracing_enabled(true)` would
    // reset the collector — stranding that open span (its `end_timing`
    // then throws "unknown timing handle") and erasing already-completed
    // sibling spans. Only reset when the collector is idle. When idle
    // (the standalone common case) this is byte-identical to today; when
    // nested, the workflow's spans simply attach under the caller's span.
    crate::tracing::enable_tracing_preserving_open_spans();
    let workflow_span_id = crate::tracing::span_start(
        crate::tracing::SpanKind::Pipeline,
        graph.name.clone().unwrap_or_else(|| graph.id.clone()),
    );
    let run_usage_before = llm_usage_snapshot();
    let report = validate_workflow(&graph, Some(&builtin_ceiling()));
    if !report.valid {
        return Err(VmError::Runtime(format!(
            "workflow_execute: invalid workflow: {}",
            report.errors.join("; ")
        )));
    }
    let workflow_verification_contracts = workflow_verification_contracts(&graph)?;

    let resumed_run = match optional_string_option(options, "resume_path") {
        Some(path) if !path.is_empty() => Some(load_run_record(std::path::Path::new(&path))?),
        _ => match options.get("resume_run") {
            Some(value) => Some(normalize_run_record(value)?),
            None => None,
        },
    };
    let replay_source = match optional_string_option(options, "replay_path") {
        Some(path) if !path.is_empty() => Some(load_run_record(std::path::Path::new(&path))?),
        _ => match options.get("replay_run") {
            Some(value) => Some(normalize_run_record(value)?),
            None => None,
        },
    };
    let replay_mode = options.get("replay_mode").and_then(|value| match value {
        VmValue::Nil => None,
        _ => {
            let rendered = value.display();
            if rendered.is_empty() || rendered == "nil" {
                None
            } else {
                Some(rendered)
            }
        }
    });

    let persist_path = optional_string_option(options, "persist_path")
        .or_else(|| optional_string_option(options, "resume_path"))
        .unwrap_or_else(|| {
            match std::env::var(crate::runtime_paths::HARN_RUN_DIR_ENV) {
                Ok(value) if !value.trim().is_empty() => crate::orchestration::default_run_dir(),
                _ => std::path::PathBuf::from(".harn-runs"),
            }
            .join(format!("{}.json", uuid::Uuid::now_v7()))
            .display()
            .to_string()
        });
    let execution = parse_execution_record(options.get("execution"))?;
    let parent_run_id = optional_string_option(options, "parent_run_id");
    let root_run_id =
        optional_string_option(options, "root_run_id").or_else(|| parent_run_id.clone());

    let mut run = resumed_run.unwrap_or_else(|| RunRecord {
        type_name: "run_record".to_string(),
        id: uuid::Uuid::now_v7().to_string(),
        workflow_id: graph.id.clone(),
        workflow_name: graph.name.clone(),
        task: task.clone(),
        status: "running".to_string(),
        started_at: uuid::Uuid::now_v7().to_string(),
        finished_at: None,
        parent_run_id: parent_run_id.clone(),
        root_run_id: root_run_id.clone(),
        stages: Vec::new(),
        transitions: Vec::new(),
        checkpoints: Vec::new(),
        pending_nodes: vec![graph.entry.clone()],
        completed_nodes: Vec::new(),
        child_runs: Vec::new(),
        artifacts: artifacts.clone(),
        handoffs: Vec::new(),
        policy: builtin_ceiling(),
        execution: execution.clone(),
        transcript: None,
        usage: None,
        replay_fixture: None,
        observability: None,
        trace_spans: Vec::new(),
        tool_recordings: Vec::new(),
        hitl_questions: Vec::new(),
        persona_runtime: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
        persisted_path: None,
    });
    let requested_mutation_scope = optional_string_option(options, "mutation_scope")
        .unwrap_or_else(|| {
            execution
                .as_ref()
                .and_then(|record| record.adapter.clone())
                .map(|adapter| {
                    if adapter == "worktree" {
                        "apply_worktree".to_string()
                    } else {
                        "read_only".to_string()
                    }
                })
                .unwrap_or_else(|| "read_only".to_string())
        });
    let mutation_approval_policy = options.get("approval_policy").and_then(|value| {
        serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(
            crate::llm::vm_value_to_json(value),
        )
        .ok()
    });
    let audit_input = options
        .get("audit")
        .cloned()
        .unwrap_or_else(|| VmValue::dict(crate::value::DictMap::new()));
    let mut mutation_session: MutationSessionRecord =
        serde_json::from_value(crate::llm::vm_value_to_json(&audit_input))
            .map_err(|e| VmError::Runtime(format!("workflow_execute: audit parse error: {e}")))?;
    mutation_session.run_id = Some(run.id.clone());
    mutation_session.execution_kind = Some("workflow".to_string());
    if mutation_session.mutation_scope.is_empty() {
        mutation_session.mutation_scope = requested_mutation_scope;
    }
    if mutation_session.approval_policy.is_none() {
        mutation_session.approval_policy = mutation_approval_policy;
    }
    mutation_session = mutation_session.normalize();
    if run.transcript.is_none() {
        if let Some(seed_transcript) = options.get("transcript") {
            run.transcript = Some(crate::llm::vm_value_to_json(seed_transcript));
        }
    }
    run.workflow_id = graph.id.clone();
    run.workflow_name = graph.name.clone();
    run.task = task;
    run.status = "running".to_string();
    run.parent_run_id = parent_run_id.or(run.parent_run_id.clone());
    if run.root_run_id.is_none() {
        run.root_run_id = root_run_id.or(Some(run.id.clone()));
    }
    if run.execution.is_none() {
        run.execution = execution;
    }
    run.metadata.insert(
        "effective_policy".to_string(),
        serde_json::to_value(&run.policy).unwrap_or_default(),
    );
    if let Some(parent_worker_id) = options
        .get("parent_worker_id")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
    {
        run.metadata.insert(
            "parent_worker_id".to_string(),
            serde_json::json!(parent_worker_id),
        );
    }
    if let Some(parent_stage_id) = options
        .get("parent_stage_id")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
    {
        run.metadata.insert(
            "parent_stage_id".to_string(),
            serde_json::json!(parent_stage_id),
        );
    }
    if matches!(options.get("delegated"), Some(VmValue::Bool(true))) {
        run.metadata
            .insert("delegated".to_string(), serde_json::json!(true));
    }
    if let Some(parent_run_id) = &run.parent_run_id {
        run.metadata.insert(
            "parent_run_id".to_string(),
            serde_json::json!(parent_run_id),
        );
    }
    if let Some(root_run_id) = &run.root_run_id {
        run.metadata
            .insert("root_run_id".to_string(), serde_json::json!(root_run_id));
    }
    if let Some(execution) = &run.execution {
        run.metadata.insert(
            "execution".to_string(),
            serde_json::to_value(execution).unwrap_or_default(),
        );
    }
    run.metadata.insert(
        "mutation_session".to_string(),
        serde_json::to_value(&mutation_session).unwrap_or_default(),
    );
    if !graph.metadata.is_empty() {
        run.metadata.insert(
            "workflow_metadata".to_string(),
            serde_json::to_value(&graph.metadata).unwrap_or_default(),
        );
    }
    let dispatch_context = crate::triggers::dispatcher::current_dispatch_context();
    let trigger_event = parse_trigger_event_option(options.get("trigger_event"))?.or_else(|| {
        dispatch_context
            .as_ref()
            .map(|context| context.trigger_event.clone())
    });
    if let Some(trigger_event) = trigger_event {
        run.metadata.insert(
            "trigger_event".to_string(),
            serde_json::to_value(&trigger_event).unwrap_or_default(),
        );
        run.metadata.insert(
            "trace_id".to_string(),
            serde_json::json!(trigger_event.trace_id.0),
        );
    }
    if let Some(replay_of_event_id) = dispatch_context
        .as_ref()
        .and_then(|context| context.replay_of_event_id.as_ref())
    {
        run.metadata.insert(
            "replay_of_event_id".to_string(),
            serde_json::json!(replay_of_event_id),
        );
    }

    let transcript = run.transcript.clone();
    if !run.artifacts.is_empty() {
        artifacts = run.artifacts.clone();
    }
    let ready_nodes = if run.pending_nodes.is_empty() {
        vec![graph.entry.clone()]
    } else {
        run.pending_nodes.clone()
    };
    let completed_nodes = run.completed_nodes.clone();
    let max_steps = options
        .get("max_steps")
        .and_then(|v| v.as_int())
        .unwrap_or((graph.nodes.len() * 4) as i64)
        .max(1) as usize;
    run.metadata.insert(
        "workflow_version".to_string(),
        serde_json::json!(graph.version),
    );
    run.metadata.insert(
        "validation".to_string(),
        serde_json::to_value(&report).unwrap_or_default(),
    );
    run.metadata
        .insert("max_steps".to_string(), serde_json::json!(max_steps));
    run.metadata.insert(
        "resumed".to_string(),
        serde_json::json!(!run.stages.is_empty()),
    );
    if let Some(replay_mode) = &replay_mode {
        run.metadata
            .insert("replay_mode".to_string(), serde_json::json!(replay_mode));
    }
    if let Some(replay_source) = &replay_source {
        run.metadata.insert(
            "replayed_from".to_string(),
            serde_json::json!(replay_source.id.clone()),
        );
    }

    let mut state = WorkflowRunState {
        state_id: uuid::Uuid::now_v7().to_string(),
        graph,
        run,
        artifacts,
        transcript,
        ready_nodes,
        completed_nodes,
        persist_path,
        replay_mode,
        replay_stages: replay_source.map(|source| source.stages),
        workflow_verification_contracts,
        mutation_session,
        run_usage_before,
        workflow_span_id,
        max_steps,
        stage_scope: None,
    };
    checkpoint_workflow_state(&mut state, None, "start")?;
    Ok(state)
}

fn stage_metadata(
    node: &crate::orchestration::WorkflowNode,
    stage_policy: &crate::orchestration::CapabilityPolicy,
    executed: &ExecutedStage,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = [
        (
            "model_policy",
            serde_json::to_value(&node.model_policy).unwrap_or_default(),
        ),
        (
            "auto_compact",
            serde_json::to_value(&node.auto_compact).unwrap_or_default(),
        ),
        (
            "context_policy",
            serde_json::to_value(&node.context_policy).unwrap_or_default(),
        ),
        (
            "retry_policy",
            serde_json::to_value(&node.retry_policy).unwrap_or_default(),
        ),
        (
            "effective_capability_policy",
            serde_json::to_value(stage_policy).unwrap_or_default(),
        ),
        (
            "input_contract",
            serde_json::to_value(&node.input_contract).unwrap_or_default(),
        ),
        (
            "output_contract",
            serde_json::to_value(&node.output_contract).unwrap_or_default(),
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect::<BTreeMap<_, _>>();
    if let Some(visibility) = &node.output_visibility {
        metadata.insert(
            "output_visibility".to_string(),
            serde_json::json!(visibility),
        );
    }
    if let Some(worker) = executed.result.get("worker") {
        metadata.insert("worker".to_string(), worker.clone());
        if let Some(worker_id) = worker.get("id") {
            metadata.insert("worker_id".to_string(), worker_id.clone());
        }
    }
    if let Some(error) = executed.error.clone() {
        metadata.insert("error".to_string(), serde_json::json!(error));
    }
    for key in [
        "prompt",
        "system_prompt",
        "rendered_context",
        "verification_contracts",
        "rendered_verification_context",
        "selected_artifact_ids",
        "selected_artifact_titles",
    ] {
        if let Some(value) = executed.result.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    if let Some(tool_calling_mode) = executed
        .result
        .get("tools")
        .and_then(|tools| tools.get("mode"))
    {
        metadata.insert("tool_calling_mode".to_string(), tool_calling_mode.clone());
    }
    metadata
}

fn is_deterministic_verify_command(node: &crate::orchestration::WorkflowNode) -> bool {
    node.kind == "verify"
        && node
            .verify
            .as_ref()
            .and_then(|verify| verify.as_object())
            .and_then(|verify| verify.get("command"))
            .and_then(|command| command.as_str())
            .map(str::trim)
            .is_some_and(|command| !command.is_empty())
}

fn workflow_stage_plan_to_vm(scope: &StageExecutionScope) -> Result<VmValue, VmError> {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("kind", scope.node.kind.as_str());
    match &scope.execution {
        StageExecution::Precomputed(executed) => {
            dict.insert(
                crate::value::intern_key("result"),
                crate::stdlib::json_to_vm_value(&executed.result),
            );
        }
        StageExecution::HarnCall { prepared, .. } => {
            if let Some(result) = &prepared.result {
                dict.insert(
                    crate::value::intern_key("result"),
                    crate::stdlib::json_to_vm_value(result),
                );
            } else {
                dict.put_str("prompt", prepared.prompt.as_str());
                dict.insert(
                    crate::value::intern_key("system"),
                    prepared
                        .system
                        .as_ref()
                        .map(|system| VmValue::String(arcstr::ArcStr::from(system.as_str())))
                        .unwrap_or(VmValue::Nil),
                );
                dict.insert(
                    crate::value::intern_key("run_agent_loop"),
                    VmValue::Bool(prepared.run_agent_loop),
                );
                dict.insert(
                    crate::value::intern_key("llm_options"),
                    VmValue::dict(prepared.llm_options.clone()),
                );
                dict.insert(
                    crate::value::intern_key("agent_loop_options"),
                    VmValue::dict(prepared.agent_loop_options.clone()),
                );
            }
        }
        StageExecution::HarnMapCall {
            artifacts,
            map_options,
            ..
        } => {
            dict.insert(
                crate::value::intern_key("run_map_stage"),
                VmValue::Bool(true),
            );
            dict.insert(crate::value::intern_key("node"), to_vm(&scope.node)?);
            dict.insert(crate::value::intern_key("artifacts"), to_vm(artifacts)?);
            dict.insert(
                crate::value::intern_key("map_options"),
                VmValue::dict(map_options.clone()),
            );
        }
    }
    Ok(VmValue::dict(dict))
}

fn workflow_stage_error_result(error: &VmError) -> serde_json::Value {
    serde_json::json!({
        "status": "failed",
        "text": "",
        "visible_text": "",
        "error": error.to_string(),
    })
}

fn harn_stage_call_error(value: &serde_json::Value) -> Option<VmError> {
    if !value
        .get("__workflow_stage_error")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    let error = value
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("workflow stage call failed");
    Some(VmError::Runtime(error.to_string()))
}

fn failed_executed_stage_from_error(
    error: VmError,
    usage_before: &UsageSnapshot,
    started_at: String,
    transcript: Option<VmValue>,
    consumed_artifact_ids: Vec<String>,
) -> ExecutedStage {
    let usage = llm_usage_delta(usage_before, &llm_usage_snapshot());
    let error_message = error.to_string();
    ExecutedStage {
        status: "failed".to_string(),
        outcome: "error".to_string(),
        branch: Some("error".to_string()),
        result: workflow_stage_error_result(&error),
        artifacts: Vec::new(),
        transcript,
        verification: None,
        usage,
        error: Some(error_message.clone()),
        attempts: vec![crate::orchestration::RunStageAttemptRecord {
            attempt: 1,
            status: "failed".to_string(),
            outcome: "error".to_string(),
            branch: Some("error".to_string()),
            error: Some(error_message),
            verification: None,
            started_at,
            finished_at: Some(uuid::Uuid::now_v7().to_string()),
        }],
        consumed_artifact_ids,
    }
}

async fn complete_harn_stage_call(
    ctx: &AsyncBuiltinCtx,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    prepared: PreparedWorkflowStageNode,
    usage_before: UsageSnapshot,
    attempt_started_at: String,
    input_transcript: Option<VmValue>,
    llm_result: serde_json::Value,
) -> Result<ExecutedStage, VmError> {
    let consumed_artifact_ids = prepared
        .selected
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    if let Some(error) = harn_stage_call_error(&llm_result) {
        return Ok(failed_executed_stage_from_error(
            error,
            &usage_before,
            attempt_started_at,
            input_transcript,
            consumed_artifact_ids,
        ));
    }
    let execution =
        crate::orchestration::complete_prepared_stage_node(node_id, node, &prepared, llm_result);
    let (result, produced, next_transcript) = match execution {
        Ok(value) => value,
        Err(error) => {
            return Ok(failed_executed_stage_from_error(
                error,
                &usage_before,
                attempt_started_at,
                input_transcript,
                consumed_artifact_ids,
            ));
        }
    };
    let (outcome, branch, verification) =
        match stage_attempt_outcome(ctx, node, &result, None).await {
            Ok(value) => value,
            Err(error) => {
                return Ok(failed_executed_stage_from_error(
                    error,
                    &usage_before,
                    attempt_started_at,
                    input_transcript,
                    consumed_artifact_ids,
                ));
            }
        };
    let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
    let success = !matches!(branch.as_deref(), Some("failed"));
    Ok(ExecutedStage {
        status: if success {
            "completed".to_string()
        } else {
            "failed".to_string()
        },
        outcome: outcome.clone(),
        branch: branch.clone(),
        result,
        artifacts: produced,
        transcript: next_transcript,
        verification: Some(verification.clone()),
        usage,
        error: if success {
            None
        } else {
            Some("verification failed".to_string())
        },
        attempts: vec![crate::orchestration::RunStageAttemptRecord {
            attempt: 1,
            status: if success {
                "completed".to_string()
            } else {
                "failed".to_string()
            },
            outcome,
            branch,
            error: None,
            verification: Some(verification),
            started_at: attempt_started_at,
            finished_at: Some(uuid::Uuid::now_v7().to_string()),
        }],
        consumed_artifact_ids,
    })
}

#[derive(Debug, serde::Deserialize)]
struct WorkflowMapStageExecution {
    result: serde_json::Value,
    artifacts: Vec<ArtifactRecord>,
    outcome: String,
    branch: Option<String>,
}

fn complete_harn_map_stage_call(
    usage_before: UsageSnapshot,
    attempt_started_at: String,
    input_transcript: Option<VmValue>,
    consumed_artifact_ids: Vec<String>,
    map_result: serde_json::Value,
) -> ExecutedStage {
    if let Some(error) = harn_stage_call_error(&map_result) {
        return failed_executed_stage_from_error(
            error,
            &usage_before,
            attempt_started_at,
            input_transcript,
            consumed_artifact_ids,
        );
    }
    let executed: Result<WorkflowMapStageExecution, _> = serde_json::from_value(map_result);
    let executed = match executed {
        Ok(value) => value,
        Err(error) => {
            return failed_executed_stage_from_error(
                VmError::Runtime(format!(
                    "workflow_execute_map_stage returned invalid shape: {error}"
                )),
                &usage_before,
                attempt_started_at,
                input_transcript,
                consumed_artifact_ids,
            )
        }
    };
    let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
    let success = !matches!(executed.branch.as_deref(), Some("failed"));
    let artifacts = executed
        .artifacts
        .into_iter()
        .map(ArtifactRecord::normalize)
        .collect::<Vec<_>>();
    ExecutedStage {
        status: if success {
            "completed".to_string()
        } else {
            "failed".to_string()
        },
        outcome: executed.outcome.clone(),
        branch: executed.branch.clone(),
        result: executed.result,
        artifacts,
        transcript: input_transcript,
        verification: None,
        usage,
        error: if success {
            None
        } else {
            Some("verification failed".to_string())
        },
        attempts: vec![crate::orchestration::RunStageAttemptRecord {
            attempt: 1,
            status: if success {
                "completed".to_string()
            } else {
                "failed".to_string()
            },
            outcome: executed.outcome,
            branch: executed.branch,
            error: None,
            verification: None,
            started_at: attempt_started_at,
            finished_at: Some(uuid::Uuid::now_v7().to_string()),
        }],
        consumed_artifact_ids,
    }
}

pub(super) async fn prepare_workflow_stage_state(
    ctx: &AsyncBuiltinCtx,
    mut state: WorkflowRunState,
    node_id: String,
    options: &crate::value::DictMap,
) -> Result<(WorkflowRunState, VmValue), VmError> {
    if state.stage_scope.is_some() {
        return Err(VmError::Runtime(format!(
            "workflow_execute: stage {} is already prepared",
            state
                .stage_scope
                .as_ref()
                .map(|scope| scope.node_id.as_str())
                .unwrap_or("unknown")
        )));
    }
    let raw_node = state
        .graph
        .nodes
        .get(&node_id)
        .cloned()
        .ok_or_else(|| VmError::Runtime(format!("workflow_execute: missing node {node_id}")))?;
    let mut node = apply_runtime_node_overrides(raw_node, options);
    crate::orchestration::inject_workflow_verification_contracts(
        &mut node,
        &state.workflow_verification_contracts,
    );
    let stage_policy = effective_node_policy(&state.graph, &node)?;
    let stage_approval = effective_node_approval_policy(&state.graph, &node);
    let stage_id = format!(
        "{}:{}:{}",
        state.run.id,
        node_id,
        state.run.stages.len() + 1
    );
    let started_at = uuid::Uuid::now_v7().to_string();

    install_current_mutation_session(Some(state.mutation_session.clone()));
    let mutation_session_guard = MutationSessionResetGuard;

    let workflow_skill_registry = options
        .get("skills")
        .cloned()
        .and_then(validate_workflow_skill_registry);
    let workflow_skill_match = options.get("skill_match").cloned();
    if workflow_skill_registry.is_some() || workflow_skill_match.is_some() {
        install_workflow_skill_context(Some(WorkflowSkillContext {
            registry: workflow_skill_registry,
            match_config: workflow_skill_match,
        }));
    }
    let workflow_skill_guard = WorkflowSkillContextGuard;

    let workflow_approval_guard = match state.mutation_session.approval_policy.clone() {
        Some(policy) => {
            crate::orchestration::push_approval_policy(policy);
            WorkflowApprovalPolicyGuard(true)
        }
        None => WorkflowApprovalPolicyGuard(false),
    };

    push_execution_policy(stage_policy.clone());
    crate::orchestration::push_approval_policy(stage_approval.clone());
    let stage_execution_policy_guard = WorkflowExecutionPolicyGuard(true);
    let stage_approval_guard = WorkflowApprovalPolicyGuard(true);
    let runtime_context_guard = crate::runtime_context::install_runtime_context_overlay(
        crate::runtime_context::RuntimeContextOverlay {
            workflow_id: Some(state.run.workflow_id.clone()),
            run_id: Some(state.run.id.clone()),
            stage_id: Some(stage_id.clone()),
            worker_id: None,
        },
    );
    let transcript = state
        .transcript
        .as_ref()
        .map(crate::stdlib::json_to_vm_value);
    let execution = if state.replay_mode.as_deref() == Some("deterministic") {
        match state.replay_stages.take() {
            Some(stages) => {
                let mut stages = VecDeque::from(stages);
                let result = replay_stage(&node_id, &mut stages);
                state.replay_stages = Some(stages.into_iter().collect());
                StageExecution::Precomputed(result?)
            }
            None => {
                return Err(VmError::Runtime(
                    "replay_mode requires replay_run or replay_path".to_string(),
                ))
            }
        }
    } else if node.kind == "map" {
        let selected_stage_artifacts = crate::orchestration::select_workflow_stage_artifacts(
            ctx,
            &state.artifacts,
            &node.context_policy,
            &node.input_contract,
        )
        .await?
        .artifacts;
        let consumed_artifact_ids = selected_stage_artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>();
        let mut map_options = crate::value::DictMap::new();
        map_options.put_str("task", state.run.task.clone());
        if let Some(transcript) = transcript.clone() {
            map_options.insert(crate::value::intern_key("transcript"), transcript);
        }
        StageExecution::HarnMapCall {
            artifacts: state.artifacts.clone(),
            map_options,
            usage_before: llm_usage_snapshot(),
            attempt_started_at: uuid::Uuid::now_v7().to_string(),
            input_transcript: transcript,
            consumed_artifact_ids,
        }
    } else if matches!(
        node.kind.as_str(),
        "subagent" | "fork" | "join" | "condition" | "reduce" | "escalation"
    ) || is_deterministic_verify_command(&node)
        || matches!(node.mode.as_deref(), Some("command" | "compact"))
        || (node.mode.as_deref() == Some("manual")
            && node
                .metadata
                .get("human_gate")
                .and_then(|value| value.as_bool())
                .unwrap_or(false))
    {
        StageExecution::Precomputed(
            execute_stage_attempts(
                ctx,
                &state.run.task,
                &node_id,
                &node,
                &state.artifacts,
                transcript.clone(),
            )
            .await?,
        )
    } else {
        let usage_before = llm_usage_snapshot();
        let attempt_started_at = uuid::Uuid::now_v7().to_string();
        let prepared = crate::orchestration::prepare_stage_node(
            ctx,
            &node_id,
            &node,
            &state.run.task,
            &state.artifacts,
        )
        .await;
        match prepared {
            Ok(prepared) => StageExecution::HarnCall {
                prepared,
                usage_before,
                attempt_started_at,
                input_transcript: transcript,
            },
            Err(error) => StageExecution::Precomputed(failed_executed_stage_from_error(
                error,
                &usage_before,
                attempt_started_at,
                transcript,
                Vec::new(),
            )),
        }
    };

    let scope = StageExecutionScope {
        node_id,
        node,
        stage_policy,
        stage_id,
        started_at,
        execution,
        _mutation_session_guard: mutation_session_guard,
        _workflow_skill_guard: workflow_skill_guard,
        _workflow_approval_guard: workflow_approval_guard,
        _stage_execution_policy_guard: stage_execution_policy_guard,
        _stage_approval_guard: stage_approval_guard,
        _runtime_context_guard: runtime_context_guard,
    };
    let plan = workflow_stage_plan_to_vm(&scope)?;
    state.stage_scope = Some(scope);
    Ok((state, plan))
}

pub(super) async fn complete_workflow_stage_state(
    ctx: &AsyncBuiltinCtx,
    mut state: WorkflowRunState,
    node_id: String,
    llm_result: serde_json::Value,
) -> Result<(WorkflowRunState, WorkflowExecutedStageRecord), VmError> {
    let scope = state.stage_scope.take().ok_or_else(|| {
        VmError::Runtime("__host_workflow_stage_complete: no prepared stage".to_string())
    })?;
    if scope.node_id != node_id {
        return Err(VmError::Runtime(format!(
            "__host_workflow_stage_complete: prepared node {} but completed {node_id}",
            scope.node_id
        )));
    }
    let StageExecutionScope {
        node_id,
        node,
        stage_policy,
        stage_id,
        started_at,
        execution,
        _mutation_session_guard,
        _workflow_skill_guard,
        _workflow_approval_guard,
        _stage_execution_policy_guard,
        _stage_approval_guard,
        _runtime_context_guard,
    } = scope;
    let executed = match execution {
        StageExecution::Precomputed(executed) => executed,
        StageExecution::HarnCall {
            prepared,
            usage_before,
            attempt_started_at,
            input_transcript,
        } => {
            complete_harn_stage_call(
                ctx,
                &node_id,
                &node,
                prepared,
                usage_before,
                attempt_started_at,
                input_transcript,
                llm_result,
            )
            .await?
        }
        StageExecution::HarnMapCall {
            usage_before,
            attempt_started_at,
            input_transcript,
            consumed_artifact_ids,
            ..
        } => complete_harn_map_stage_call(
            usage_before,
            attempt_started_at,
            input_transcript,
            consumed_artifact_ids,
            llm_result,
        ),
    };
    state.transcript = executed
        .transcript
        .as_ref()
        .map(crate::llm::vm_value_to_json);
    state.artifacts.extend(executed.artifacts.clone());
    state.run.artifacts = state.artifacts.clone();
    state.run.transcript = state.transcript.clone();

    let produced_artifact_ids = executed
        .artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    let metadata = stage_metadata(&node, &stage_policy, &executed);
    state.run.stages.push(RunStageRecord {
        id: stage_id.clone(),
        node_id: node_id.clone(),
        kind: node.kind.clone(),
        status: executed.status.clone(),
        outcome: executed.outcome.clone(),
        branch: executed.branch.clone(),
        started_at,
        finished_at: Some(uuid::Uuid::now_v7().to_string()),
        visible_text: executed
            .result
            .get("visible_text")
            .or_else(|| executed.result.get("text"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        private_reasoning: executed
            .result
            .get("private_reasoning")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        transcript: executed
            .transcript
            .as_ref()
            .map(crate::llm::vm_value_to_json),
        verification: executed.verification.clone(),
        usage: Some(executed.usage.clone()),
        artifacts: executed.artifacts.clone(),
        consumed_artifact_ids: executed.consumed_artifact_ids.clone(),
        produced_artifact_ids: produced_artifact_ids.clone(),
        attempts: executed.attempts,
        metadata,
    });
    append_child_run_record(&mut state.run, &stage_id, &executed.result);
    if !state
        .completed_nodes
        .iter()
        .any(|completed| completed == &node_id)
    {
        state.completed_nodes.push(node_id.clone());
    }

    let stage = WorkflowExecutedStageRecord {
        stage_id,
        node_id,
        branch: executed.branch,
        consumed_artifact_ids: executed.consumed_artifact_ids,
        produced_artifact_ids,
    };
    Ok((state, stage))
}

pub(super) fn record_workflow_transitions(
    mut state: WorkflowRunState,
    stage: WorkflowExecutedStageRecord,
    edges: Vec<WorkflowEdge>,
) -> Result<WorkflowRunState, VmError> {
    for edge in edges {
        state.run.transitions.push(RunTransitionRecord {
            id: uuid::Uuid::now_v7().to_string(),
            from_stage_id: Some(stage.stage_id.clone()),
            from_node_id: Some(stage.node_id.clone()),
            to_node_id: edge.to,
            branch: edge.branch.clone(),
            timestamp: uuid::Uuid::now_v7().to_string(),
            consumed_artifact_ids: stage.consumed_artifact_ids.clone(),
            produced_artifact_ids: stage.produced_artifact_ids.clone(),
        });
    }
    checkpoint_workflow_state(&mut state, Some(stage.stage_id), "stage_complete")?;
    Ok(state)
}

pub(super) fn finalize_workflow_state(mut state: WorkflowRunState) -> Result<VmValue, VmError> {
    state.run.status = if state.ready_nodes.is_empty() {
        "completed".to_string()
    } else {
        "paused".to_string()
    };
    state.run.finished_at = Some(uuid::Uuid::now_v7().to_string());
    state.run.usage = Some(llm_usage_delta(
        &state.run_usage_before,
        &llm_usage_snapshot(),
    ));
    state.run.replay_fixture = Some(crate::orchestration::replay_fixture_from_run(&state.run));
    crate::tracing::span_end(state.workflow_span_id);
    state.run.trace_spans = snapshot_trace_spans();
    state.run.tool_recordings = crate::llm::mock::drain_tool_recordings();
    checkpoint_workflow_state(&mut state, None, "finalize")?;

    to_vm(&serde_json::json!({
        "status": state.run.status,
        "run": state.run,
        "artifacts": state.artifacts,
        "transcript": state.transcript,
        "path": state.persist_path,
    }))
}
