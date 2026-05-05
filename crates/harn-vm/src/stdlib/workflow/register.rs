//! Top-level workflow executor and builtin registration.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use crate::orchestration::{
    append_audit_entry, builtin_ceiling, install_current_mutation_session,
    install_workflow_skill_context, load_run_record, normalize_run_record,
    normalize_workflow_value, pop_execution_policy, push_execution_policy, validate_workflow,
    workflow_verification_contracts, ArtifactRecord, MutationSessionRecord, RunRecord,
    RunStageRecord, RunTransitionRecord, VerificationContract, WorkflowEdge, WorkflowGraph,
    WorkflowSkillContext, WorkflowSkillContextGuard,
};
use crate::stdlib::harn_entry::register_harn_entrypoint_category;
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

use super::super::{parse_artifact_list, parse_context_policy};

use super::artifact::{
    append_child_run_record, artifact_from_value, checkpoint_run, optional_string_option,
    parse_execution_record, snapshot_trace_spans,
};
use super::convert::{to_vm, workflow_graph_to_vm};
use super::guards::{MutationSessionResetGuard, WorkflowApprovalPolicyGuard};
use super::map::{
    map_branch_artifact, map_execution_plan, map_finalize, MapBranchResult, MapExecutionPlan,
    MapWorkItem,
};
use super::policy::{
    apply_runtime_node_overrides, effective_node_approval_policy, effective_node_policy,
    normalize_policy, set_node_policy,
};
use super::stage::{execute_stage_attempts, replay_stage, ExecutedStage};
use super::usage::{llm_usage_delta, llm_usage_snapshot, UsageSnapshot};

const WORKFLOW_STDLIB_ENTRYPOINT_CATEGORY: &str = "workflow.stdlib";
const HOST_WORKFLOW_PREPARE_RUN_BUILTIN: &str = "__host_workflow_prepare_run";
const HOST_WORKFLOW_EXECUTE_STAGE_BUILTIN: &str = "__host_workflow_execute_stage";
const HOST_WORKFLOW_RECORD_TRANSITIONS_BUILTIN: &str = "__host_workflow_record_transitions";
const HOST_WORKFLOW_FINALIZE_RUN_BUILTIN: &str = "__host_workflow_finalize_run";
const HOST_WORKFLOW_MAP_PLAN_BUILTIN: &str = "__host_workflow_map_plan";
const HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN: &str = "__host_workflow_map_branch_artifact";
const HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN: &str = "__host_workflow_map_execute_branch";
const HOST_WORKFLOW_MAP_FINALIZE_BUILTIN: &str = "__host_workflow_map_finalize";
type PostHookFn = Rc<dyn Fn(&str, &str) -> crate::orchestration::PostToolAction>;

thread_local! {
    static WORKFLOW_RUN_STATES: RefCell<BTreeMap<String, WorkflowRunState>> =
        const { RefCell::new(BTreeMap::new()) };
}

const WORKFLOW_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("workflow_graph", workflow_graph_builtin)
        .signature("workflow_graph(input?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Normalize a workflow value and return the canonical workflow graph dict."),
    SyncBuiltin::new("workflow_validate", workflow_validate_builtin)
        .signature("workflow_validate(input?, ceiling?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 2 })
        .doc("Validate a workflow graph against a capability policy ceiling."),
    SyncBuiltin::new("workflow_inspect", workflow_inspect_builtin)
        .signature("workflow_inspect(input?, ceiling?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 2 })
        .doc("Return normalized workflow graph shape and validation details."),
    SyncBuiltin::new("workflow_policy_report", workflow_policy_report_builtin)
        .signature("workflow_policy_report(graph, ceiling?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Report workflow and node policies against an effective ceiling."),
    SyncBuiltin::new("workflow_clone", workflow_clone_builtin)
        .signature("workflow_clone(graph)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Clone a workflow graph and append audit metadata."),
    SyncBuiltin::new("workflow_insert_node", workflow_insert_node_builtin)
        .signature("workflow_insert_node(graph, node, edge?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Insert a node and optional edge into a workflow graph."),
    SyncBuiltin::new("workflow_replace_node", workflow_replace_node_builtin)
        .signature("workflow_replace_node(graph, node_id, node)")
        .arity(VmBuiltinArity::Exact(3))
        .doc("Replace one node in a workflow graph."),
    SyncBuiltin::new("workflow_rewire", workflow_rewire_builtin)
        .signature("workflow_rewire(graph, from, to, branch?)")
        .arity(VmBuiltinArity::Range { min: 3, max: 4 })
        .doc("Replace outgoing edge wiring for one workflow graph node."),
    SyncBuiltin::new(
        "workflow_set_model_policy",
        workflow_set_model_policy_builtin,
    )
    .signature("workflow_set_model_policy(graph, node_id, policy)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's model policy."),
    SyncBuiltin::new(
        "workflow_set_context_policy",
        workflow_set_context_policy_builtin,
    )
    .signature("workflow_set_context_policy(graph, node_id, policy)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's context policy."),
    SyncBuiltin::new(
        "workflow_set_auto_compact",
        workflow_set_auto_compact_builtin,
    )
    .signature("workflow_set_auto_compact(graph, node_id, policy)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's auto-compaction policy."),
    SyncBuiltin::new(
        "workflow_set_output_visibility",
        workflow_set_output_visibility_builtin,
    )
    .signature("workflow_set_output_visibility(graph, node_id, visibility)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's output visibility policy."),
    SyncBuiltin::new("workflow_diff", workflow_diff_builtin)
        .signature("workflow_diff(left, right)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Compare two workflow graph values for canonical JSON changes."),
    SyncBuiltin::new("workflow_commit", workflow_commit_builtin)
        .signature("workflow_commit(graph, reason?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Validate and commit workflow graph audit metadata."),
    SyncBuiltin::new("register_tool_hook", register_tool_hook_builtin)
        .signature("register_tool_hook(config?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Register low-level pre/post tool hooks for workflow execution."),
    SyncBuiltin::new("clear_tool_hooks", clear_tool_hooks_builtin)
        .signature("clear_tool_hooks()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear registered low-level workflow tool hooks."),
    SyncBuiltin::new(
        "select_artifacts_adaptive",
        select_artifacts_adaptive_builtin,
    )
    .signature("select_artifacts_adaptive(artifacts?, policy?)")
    .arity(VmBuiltinArity::Range { min: 0, max: 2 })
    .doc("Select workflow artifacts according to a context policy."),
    SyncBuiltin::new("estimate_tokens", estimate_tokens_builtin)
        .signature("estimate_tokens(messages?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Estimate tokens for a list of message objects."),
    SyncBuiltin::new("microcompact", microcompact_builtin)
        .signature("microcompact(text, max_chars?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Compact long tool output with the host microcompaction primitive."),
    SyncBuiltin::new(
        HOST_WORKFLOW_PREPARE_RUN_BUILTIN,
        host_workflow_prepare_run_builtin,
    )
    .signature("__host_workflow_prepare_run(task, graph, artifacts?, options?)")
    .arity(VmBuiltinArity::Range { min: 2, max: 4 })
    .doc("Prepare low-level workflow run state for the Harn stdlib workflow executor."),
    SyncBuiltin::new(
        HOST_WORKFLOW_RECORD_TRANSITIONS_BUILTIN,
        host_workflow_record_transitions_builtin,
    )
    .signature("__host_workflow_record_transitions(state_id, ready_nodes, stage, edges)")
    .arity(VmBuiltinArity::Exact(4))
    .doc("Record workflow stage transitions and checkpoint low-level run state."),
    SyncBuiltin::new(
        HOST_WORKFLOW_FINALIZE_RUN_BUILTIN,
        host_workflow_finalize_run_builtin,
    )
    .signature("__host_workflow_finalize_run(state_id, ready_nodes)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Finalize low-level workflow run state and persist the final checkpoint."),
    SyncBuiltin::new(
        HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN,
        host_workflow_map_branch_artifact_builtin,
    )
    .signature("__host_workflow_map_branch_artifact(node_id, item, lineage)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Build the synthesized input artifact for one Harn-owned workflow map branch."),
];

const WORKFLOW_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!(
        HOST_WORKFLOW_EXECUTE_STAGE_BUILTIN,
        host_workflow_execute_stage_builtin
    )
    .signature("__host_workflow_execute_stage(state_id, node_id, ready_nodes, options?)")
    .arity(VmBuiltinArity::Range { min: 3, max: 4 })
    .doc("Execute one low-level workflow stage primitive for the Harn stdlib workflow executor."),
    async_builtin!(
        HOST_WORKFLOW_MAP_PLAN_BUILTIN,
        host_workflow_map_plan_builtin
    )
    .signature("__host_workflow_map_plan(node, artifacts)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Return the host-normalized execution plan for a workflow map stage."),
    async_builtin!(
        HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN,
        host_workflow_map_execute_branch_builtin
    )
    .signature("__host_workflow_map_execute_branch(node_id, plan, item, branch_artifact, options?)")
    .arity(VmBuiltinArity::Range { min: 4, max: 5 })
    .doc("Execute one workflow map branch while Harn owns branch scheduling."),
    async_builtin!(
        HOST_WORKFLOW_MAP_FINALIZE_BUILTIN,
        host_workflow_map_finalize_builtin
    )
    .signature("__host_workflow_map_finalize(strategy, total_items, completed, failures, produced)")
    .arity(VmBuiltinArity::Exact(5))
    .doc("Finalize a Harn-owned workflow map stage after branch settlement."),
    async_builtin!("transcript_auto_compact", transcript_auto_compact_builtin)
        .signature("transcript_auto_compact(messages, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Apply the workflow/agent transcript auto-compaction primitive to a message list."),
];

const WORKFLOW_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("workflow.host")
    .sync(WORKFLOW_SYNC_PRIMITIVES)
    .async_(WORKFLOW_ASYNC_PRIMITIVES);

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
            let mut dict = BTreeMap::new();
            dict.insert(
                "_type".to_string(),
                VmValue::String(Rc::from("skill_registry")),
            );
            dict.insert("skills".to_string(), VmValue::List(list.clone()));
            Some(VmValue::Dict(Rc::new(dict)))
        }
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct WorkflowRunState {
    state_id: String,
    graph: WorkflowGraph,
    run: RunRecord,
    artifacts: Vec<ArtifactRecord>,
    transcript: Option<serde_json::Value>,
    ready_nodes: Vec<String>,
    completed_nodes: Vec<String>,
    persist_path: String,
    replay_mode: Option<String>,
    replay_stages: Option<Vec<RunStageRecord>>,
    workflow_verification_contracts: Vec<VerificationContract>,
    mutation_session: MutationSessionRecord,
    run_usage_before: UsageSnapshot,
    workflow_span_id: u64,
    max_steps: usize,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WorkflowExecutedStageRecord {
    stage_id: String,
    node_id: String,
    branch: Option<String>,
    consumed_artifact_ids: Vec<String>,
    produced_artifact_ids: Vec<String>,
}

fn parse_options_arg(args: &[VmValue], index: usize) -> BTreeMap<String, VmValue> {
    args.get(index)
        .and_then(|v| v.as_dict())
        .cloned()
        .unwrap_or_default()
}

fn string_list_to_vm(values: &[String]) -> VmValue {
    VmValue::List(Rc::new(
        values
            .iter()
            .map(|value| VmValue::String(Rc::from(value.as_str())))
            .collect(),
    ))
}

fn parse_state_id_arg(value: Option<&VmValue>, context: &str) -> Result<String, VmError> {
    value
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{context}: missing workflow state id")))
}

fn parse_string_list_arg(value: Option<&VmValue>, context: &str) -> Result<Vec<String>, VmError> {
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

fn parse_json_arg<T: serde::de::DeserializeOwned>(
    value: Option<&VmValue>,
    context: &str,
) -> Result<T, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value.unwrap_or(&VmValue::Nil)))
        .map_err(|error| VmError::Runtime(format!("{context}: invalid value: {error}")))
}

fn map_item_index(item: &MapWorkItem) -> usize {
    match item {
        MapWorkItem::Artifact { index, .. } | MapWorkItem::Value { index, .. } => *index,
    }
}

fn workflow_control_to_vm(
    state: &WorkflowRunState,
    include_graph: bool,
) -> Result<VmValue, VmError> {
    let mut dict = BTreeMap::new();
    dict.insert(
        "state_id".to_string(),
        VmValue::String(Rc::from(state.state_id.as_str())),
    );
    dict.insert(
        "run_id".to_string(),
        VmValue::String(Rc::from(state.run.id.as_str())),
    );
    dict.insert(
        "ready_nodes".to_string(),
        string_list_to_vm(&state.ready_nodes),
    );
    dict.insert(
        "completed_nodes".to_string(),
        string_list_to_vm(&state.completed_nodes),
    );
    dict.insert(
        "max_steps".to_string(),
        VmValue::Int(state.max_steps as i64),
    );
    if include_graph {
        dict.insert("graph".to_string(), workflow_graph_to_vm(&state.graph)?);
    }
    Ok(VmValue::Dict(Rc::new(dict)))
}

fn insert_workflow_state(state: WorkflowRunState) {
    WORKFLOW_RUN_STATES.with(|states| {
        states.borrow_mut().insert(state.state_id.clone(), state);
    });
}

fn remove_workflow_state(state_id: &str, context: &str) -> Result<WorkflowRunState, VmError> {
    WORKFLOW_RUN_STATES.with(|states| {
        states.borrow_mut().remove(state_id).ok_or_else(|| {
            VmError::Runtime(format!("{context}: unknown workflow state id {state_id}"))
        })
    })
}

fn parse_executed_stage_record(
    value: &VmValue,
    context: &str,
) -> Result<WorkflowExecutedStageRecord, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map_err(|error| VmError::Runtime(format!("{context}: invalid stage record: {error}")))
}

fn checkpoint_workflow_state(
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

fn prepare_workflow_state(
    task: String,
    graph: WorkflowGraph,
    mut artifacts: Vec<ArtifactRecord>,
    options: &BTreeMap<String, VmValue>,
) -> Result<WorkflowRunState, VmError> {
    crate::llm::enable_tracing();
    crate::tracing::set_tracing_enabled(true);
    let workflow_span_id = crate::tracing::span_start(
        crate::tracing::SpanKind::Pipeline,
        graph
            .name
            .clone()
            .unwrap_or_else(|| graph.id.clone())
            .to_string(),
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
        metadata: BTreeMap::new(),
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
        .unwrap_or_else(|| VmValue::Dict(Rc::new(BTreeMap::new())));
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
    run.task = task.clone();
    run.status = "running".to_string();
    run.parent_run_id = parent_run_id.clone().or(run.parent_run_id.clone());
    if run.root_run_id.is_none() {
        run.root_run_id = root_run_id.clone().or(Some(run.id.clone()));
    }
    if run.execution.is_none() {
        run.execution = execution.clone();
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

async fn execute_workflow_stage_state(
    mut state: WorkflowRunState,
    node_id: String,
    options: &BTreeMap<String, VmValue>,
) -> Result<(WorkflowRunState, WorkflowExecutedStageRecord), VmError> {
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
    let _mutation_session_guard = MutationSessionResetGuard;

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
    let _workflow_skill_guard = WorkflowSkillContextGuard;

    let workflow_approval_guard = match state.mutation_session.approval_policy.clone() {
        Some(policy) => {
            crate::orchestration::push_approval_policy(policy);
            WorkflowApprovalPolicyGuard(true)
        }
        None => WorkflowApprovalPolicyGuard(false),
    };
    let _workflow_approval_guard = workflow_approval_guard;

    push_execution_policy(stage_policy.clone());
    crate::orchestration::push_approval_policy(stage_approval.clone());
    let _runtime_context_guard = crate::runtime_context::install_runtime_context_overlay(
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
    let executed_result = if state.replay_mode.as_deref() == Some("deterministic") {
        match state.replay_stages.take() {
            Some(stages) => {
                let mut stages = VecDeque::from(stages);
                let result = replay_stage(&node_id, &mut stages);
                state.replay_stages = Some(stages.into_iter().collect());
                result
            }
            None => Err(VmError::Runtime(
                "replay_mode requires replay_run or replay_path".to_string(),
            )),
        }
    } else {
        execute_stage_attempts(
            &state.run.task,
            &node_id,
            &node,
            &state.artifacts,
            transcript,
        )
        .await
    };
    crate::orchestration::pop_approval_policy();
    pop_execution_policy();
    let executed = executed_result?;

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

fn record_workflow_transitions(
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

fn finalize_workflow_state(mut state: WorkflowRunState) -> Result<VmValue, VmError> {
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

fn host_workflow_prepare_run_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let task = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let graph = normalize_workflow_value(
        args.get(1)
            .ok_or_else(|| VmError::Runtime("workflow_execute: missing workflow".to_string()))?,
    )?;
    let artifacts = parse_artifact_list(args.get(2))?;
    let options = parse_options_arg(args, 3);
    let state = prepare_workflow_state(task, graph, artifacts, &options)?;
    let control = workflow_control_to_vm(&state, true)?;
    insert_workflow_state(state);
    Ok(control)
}

async fn host_workflow_execute_stage_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let state_id = parse_state_id_arg(args.first(), "__host_workflow_execute_stage")?;
    let mut state = remove_workflow_state(&state_id, "__host_workflow_execute_stage")?;
    let node_id = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("__host_workflow_execute_stage: missing node id".to_string())
        })?;
    state.ready_nodes =
        parse_string_list_arg(args.get(2), "__host_workflow_execute_stage ready_nodes")?;
    let options = parse_options_arg(&args, 3);
    let (state, stage) = execute_workflow_stage_state(state, node_id, &options).await?;
    let branch = stage.branch.clone();
    let control = workflow_control_to_vm(&state, false)?;
    insert_workflow_state(state);
    let mut dict = BTreeMap::new();
    dict.insert("state".to_string(), control);
    dict.insert("stage".to_string(), to_vm(&stage)?);
    dict.insert(
        "branch".to_string(),
        branch
            .map(|branch| VmValue::String(Rc::from(branch)))
            .unwrap_or(VmValue::Nil),
    );
    Ok(VmValue::Dict(Rc::new(dict)))
}

fn host_workflow_record_transitions_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let state_id = parse_state_id_arg(args.first(), "__host_workflow_record_transitions")?;
    let mut state = remove_workflow_state(&state_id, "__host_workflow_record_transitions")?;
    state.ready_nodes = parse_string_list_arg(
        args.get(1),
        "__host_workflow_record_transitions ready_nodes",
    )?;
    let stage = parse_executed_stage_record(
        args.get(2).ok_or_else(|| {
            VmError::Runtime("__host_workflow_record_transitions: missing stage".to_string())
        })?,
        "__host_workflow_record_transitions",
    )?;
    let edges: Vec<WorkflowEdge> = serde_json::from_value(crate::llm::vm_value_to_json(
        args.get(3).unwrap_or(&VmValue::Nil),
    ))
    .map_err(|error| {
        VmError::Runtime(format!(
            "__host_workflow_record_transitions: invalid edges: {error}"
        ))
    })?;
    state = record_workflow_transitions(state, stage, edges)?;
    let control = workflow_control_to_vm(&state, false)?;
    insert_workflow_state(state);
    Ok(control)
}

fn host_workflow_finalize_run_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let state_id = parse_state_id_arg(args.first(), "__host_workflow_finalize_run")?;
    let mut state = remove_workflow_state(&state_id, "__host_workflow_finalize_run")?;
    state.ready_nodes =
        parse_string_list_arg(args.get(1), "__host_workflow_finalize_run ready_nodes")?;
    finalize_workflow_state(state)
}

async fn host_workflow_map_plan_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let node: crate::orchestration::WorkflowNode =
        parse_json_arg(args.first(), HOST_WORKFLOW_MAP_PLAN_BUILTIN)?;
    let artifacts = parse_artifact_list(args.get(1))?;
    let plan = map_execution_plan(&node, &artifacts).await?;
    to_vm(&plan)
}

fn host_workflow_map_branch_artifact_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let node_id = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN}: missing node id"
            ))
        })?;
    let item: MapWorkItem = parse_json_arg(args.get(1), HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN)?;
    let lineage =
        parse_string_list_arg(args.get(2), "__host_workflow_map_branch_artifact lineage")?;
    to_vm(&map_branch_artifact(&node_id, &item, &lineage).normalize())
}

async fn host_workflow_map_execute_branch_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let node_id = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN}: missing node id"
            ))
        })?;
    let plan: MapExecutionPlan =
        parse_json_arg(args.get(1), HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN)?;
    let item: MapWorkItem = parse_json_arg(args.get(2), HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN)?;
    let branch_artifact: ArtifactRecord =
        parse_json_arg(args.get(3), HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN)?;
    let options = parse_options_arg(&args, 4);
    let index = map_item_index(&item);
    let branch = if let Some(stage_node) = plan.stage_node {
        let task_label = options
            .get("task")
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("workflow map {node_id}"));
        let transcript = options.get("transcript").cloned();
        let branch_task = format!(
            "{task_label}\n\nMap item {} of {}",
            index + 1,
            plan.total_items.max(1)
        );
        let executed = execute_stage_attempts(
            &branch_task,
            &format!("{node_id}_map_{}", index + 1),
            &stage_node,
            &[branch_artifact.normalize()],
            transcript,
        )
        .await?;
        MapBranchResult {
            index,
            status: executed.status,
            result: executed.result,
            artifacts: executed.artifacts,
            usage: executed.usage,
            error: executed.error,
        }
    } else {
        let artifact = match &item {
            MapWorkItem::Artifact { artifact, .. } => {
                let value = artifact
                    .data
                    .clone()
                    .or_else(|| artifact.text.clone().map(serde_json::Value::String))
                    .unwrap_or(serde_json::Value::Null);
                artifact_from_value(
                    &node_id,
                    &plan.output_kind,
                    index,
                    value,
                    std::slice::from_ref(&artifact.id),
                    format!("map {} item {}", node_id, index + 1),
                )
            }
            MapWorkItem::Value { value, .. } => artifact_from_value(
                &node_id,
                &plan.output_kind,
                index,
                value.clone(),
                &plan.lineage,
                format!("map {} item {}", node_id, index + 1),
            ),
        };
        MapBranchResult {
            index,
            status: "completed".to_string(),
            result: serde_json::json!({
                "status": "completed",
                "text": artifact.text,
            }),
            artifacts: vec![artifact],
            usage: Default::default(),
            error: None,
        }
    };
    to_vm(&branch)
}

async fn host_workflow_map_finalize_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let strategy = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "all".to_string());
    let total_items = match args.get(1) {
        Some(VmValue::Int(value)) => (*value).max(0) as usize,
        Some(value) => {
            return Err(VmError::Runtime(format!(
                "{HOST_WORKFLOW_MAP_FINALIZE_BUILTIN}: total_items must be an int, got {}",
                value.type_name()
            )))
        }
        None => 0,
    };
    let completed: Vec<serde_json::Value> =
        parse_json_arg(args.get(2), HOST_WORKFLOW_MAP_FINALIZE_BUILTIN)?;
    let failures: Vec<serde_json::Value> =
        parse_json_arg(args.get(3), HOST_WORKFLOW_MAP_FINALIZE_BUILTIN)?;
    let produced = parse_artifact_list(args.get(4))?;
    let (result, outcome, branch) =
        map_finalize(&strategy, total_items, produced.len(), completed, failures).await?;
    to_vm(&serde_json::json!({
        "result": result,
        "outcome": outcome,
        "branch": branch,
    }))
}

pub(crate) fn register_workflow_builtins(vm: &mut Vm) {
    register_builtin_group(vm, WORKFLOW_PRIMITIVES);
    register_harn_entrypoint_category(vm, WORKFLOW_STDLIB_ENTRYPOINT_CATEGORY);
}

fn workflow_graph_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    workflow_graph_to_vm(&graph)
}

fn workflow_validate_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    let ceiling = args.get(1).map(normalize_policy).transpose()?;
    to_vm(&validate_workflow(
        &graph,
        ceiling.as_ref().or(Some(&builtin_ceiling())),
    ))
}

fn workflow_inspect_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    let ceiling = args.get(1).map(normalize_policy).transpose()?;
    let builtin = builtin_ceiling();
    let report = validate_workflow(&graph, ceiling.as_ref().or(Some(&builtin)));
    to_vm(&serde_json::json!({
        "graph": graph,
        "validation": report,
        "node_count": graph.nodes.len(),
        "edge_count": graph.edges.len(),
    }))
}

fn workflow_policy_report_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let graph = normalize_workflow_value(&input)?;
    let ceiling = args.get(1).map(normalize_policy).transpose()?;
    let builtin = builtin_ceiling();
    let effective_ceiling = ceiling.unwrap_or(builtin);
    let report = validate_workflow(&graph, Some(&effective_ceiling));
    to_vm(&serde_json::json!({
        "workflow_policy": graph.capability_policy,
        "ceiling": effective_ceiling,
        "validation": report,
            "nodes": graph.nodes.iter().map(|(node_id, node)| serde_json::json!({
            "node_id": node_id,
            "policy": node.capability_policy,
            "tools": node.tools,
        })).collect::<Vec<_>>(),
    }))
}

fn workflow_clone_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = args
        .first()
        .cloned()
        .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
    let mut graph = normalize_workflow_value(&input)?;
    graph.id = format!("{}_clone", graph.id);
    graph.version += 1;
    append_audit_entry(&mut graph, "clone", None, None, BTreeMap::new());
    workflow_graph_to_vm(&graph)
}

fn workflow_insert_node_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut graph =
        normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_insert_node: missing workflow".to_string())
        })?)?;
    let node_value = args
        .get(1)
        .ok_or_else(|| VmError::Runtime("workflow_insert_node: missing node".to_string()))?;
    let mut node =
        crate::orchestration::parse_workflow_node_value(node_value, "workflow_insert_node")?;
    let node_id = node
        .id
        .clone()
        .or_else(|| {
            node_value
                .as_dict()
                .and_then(|d| d.get("id"))
                .map(|v| v.display())
        })
        .unwrap_or_else(|| format!("node_{}", graph.nodes.len() + 1));
    node.id = Some(node_id.clone());
    graph.nodes.insert(node_id.clone(), node);
    if let Some(VmValue::Dict(edge_dict)) = args.get(2) {
        let edge_json = crate::llm::vm_value_to_json(&VmValue::Dict(edge_dict.clone()));
        let edge =
            crate::orchestration::parse_workflow_edge_json(edge_json, "workflow_insert_node edge")?;
        graph.edges.push(edge);
    }
    append_audit_entry(
        &mut graph,
        "insert_node",
        Some(node_id),
        None,
        BTreeMap::new(),
    );
    workflow_graph_to_vm(&graph)
}

fn workflow_replace_node_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut graph =
        normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_replace_node: missing workflow".to_string())
        })?)?;
    let node_id = args
        .get(1)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow_replace_node: missing node id".to_string()))?;
    let mut node = crate::orchestration::parse_workflow_node_value(
        args.get(2)
            .ok_or_else(|| VmError::Runtime("workflow_replace_node: missing node".to_string()))?,
        "workflow_replace_node",
    )?;
    node.id = Some(node_id.clone());
    graph.nodes.insert(node_id.clone(), node);
    append_audit_entry(
        &mut graph,
        "replace_node",
        Some(node_id),
        None,
        BTreeMap::new(),
    );
    workflow_graph_to_vm(&graph)
}

fn workflow_rewire_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut graph = normalize_workflow_value(
        args.first()
            .ok_or_else(|| VmError::Runtime("workflow_rewire: missing workflow".to_string()))?,
    )?;
    let from = args
        .get(1)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow_rewire: missing from".to_string()))?;
    let to = args
        .get(2)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow_rewire: missing to".to_string()))?;
    let branch = args.get(3).map(|v| v.display()).filter(|s| !s.is_empty());
    graph
        .edges
        .retain(|edge| !(edge.from == from && edge.branch == branch));
    graph.edges.push(WorkflowEdge {
        from: from.clone(),
        to,
        branch,
        label: None,
    });
    append_audit_entry(&mut graph, "rewire", Some(from), None, BTreeMap::new());
    workflow_graph_to_vm(&graph)
}

fn workflow_set_model_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.model_policy = serde_json::from_value(policy)
            .map_err(|e| VmError::Runtime(format!("workflow_set_model_policy: {e}")))?;
        Ok(())
    })
}

fn workflow_set_context_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.context_policy = serde_json::from_value(policy)
            .map_err(|e| VmError::Runtime(format!("workflow_set_context_policy: {e}")))?;
        Ok(())
    })
}

fn workflow_set_auto_compact_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.auto_compact = serde_json::from_value(policy)
            .map_err(|e| VmError::Runtime(format!("workflow_set_auto_compact: {e}")))?;
        Ok(())
    })
}

fn workflow_set_output_visibility_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    set_node_policy(args, |node, policy| {
        node.output_visibility = match policy {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s),
            _ => {
                return Err(VmError::Runtime(
                    "workflow_set_output_visibility: value must be a string or nil".into(),
                ))
            }
        };
        Ok(())
    })
}

fn workflow_diff_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let left =
        normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_diff: missing left workflow".to_string())
        })?)?;
    let right =
        normalize_workflow_value(args.get(1).ok_or_else(|| {
            VmError::Runtime("workflow_diff: missing right workflow".to_string())
        })?)?;
    let left_json = serde_json::to_value(&left).unwrap_or_default();
    let right_json = serde_json::to_value(&right).unwrap_or_default();
    to_vm(&serde_json::json!({
        "changed": left_json != right_json,
        "left": left,
        "right": right,
    }))
}

fn workflow_commit_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut graph = normalize_workflow_value(
        args.first()
            .ok_or_else(|| VmError::Runtime("workflow_commit: missing workflow".to_string()))?,
    )?;
    let reason = args.get(1).map(|v| v.display()).filter(|s| !s.is_empty());
    let report = validate_workflow(&graph, Some(&builtin_ceiling()));
    if !report.valid {
        return Err(VmError::Runtime(format!(
            "workflow_commit: invalid workflow: {}",
            report.errors.join("; ")
        )));
    }
    append_audit_entry(&mut graph, "commit", None, reason, BTreeMap::new());
    workflow_graph_to_vm(&graph)
}

fn register_tool_hook_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = args
        .first()
        .and_then(|a| a.as_dict())
        .cloned()
        .unwrap_or_default();
    let pattern = config
        .get("pattern")
        .map(|v| v.display())
        .unwrap_or_else(|| "*".to_string());
    let deny_reason = config.get("deny").map(|v| v.display());
    let max_output = config.get("max_output").and_then(|v| match v {
        VmValue::Int(n) => Some(*n as usize),
        _ => None,
    });

    let pre: Option<crate::orchestration::PreToolHookFn> = deny_reason.map(|reason| {
        Rc::new(move |_name: &str, _args: &serde_json::Value| {
            crate::orchestration::PreToolAction::Deny(reason.clone())
        }) as _
    });

    let post: Option<PostHookFn> = max_output.map(|max| {
        Rc::new(move |_name: &str, result: &str| {
            if result.len() > max {
                crate::orchestration::PostToolAction::Modify(
                    crate::orchestration::microcompact_tool_output(result, max),
                )
            } else {
                crate::orchestration::PostToolAction::Pass
            }
        }) as _
    });

    crate::orchestration::register_tool_hook(crate::orchestration::ToolHook { pattern, pre, post });
    Ok(VmValue::Nil)
}

fn clear_tool_hooks_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::orchestration::clear_tool_hooks();
    Ok(VmValue::Nil)
}

fn select_artifacts_adaptive_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let artifacts_val = args.first().cloned().unwrap_or(VmValue::Nil);
    let policy_val = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let artifacts: Vec<ArtifactRecord> = parse_artifact_list(Some(&artifacts_val))?;
    let policy: crate::orchestration::ContextPolicy = parse_context_policy(Some(&policy_val))?;
    let selected = crate::orchestration::select_artifacts_adaptive(artifacts, &policy);
    to_vm(&selected)
}

fn estimate_tokens_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let messages: Vec<serde_json::Value> = args
        .first()
        .and_then(|a| match a {
            VmValue::List(list) => Some(
                list.iter()
                    .map(crate::llm::helpers::vm_value_to_json)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let tokens = crate::orchestration::estimate_message_tokens(&messages);
    Ok(VmValue::Int(tokens as i64))
}

fn microcompact_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let max_chars = args
        .get(1)
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(20_000);
    Ok(VmValue::String(Rc::from(
        crate::orchestration::microcompact_tool_output(&text, max_chars),
    )))
}

async fn transcript_auto_compact_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let mut messages: Vec<serde_json::Value> = match args.first() {
        Some(VmValue::List(list)) => list
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
        _ => {
            return Err(VmError::Runtime(
                "transcript_auto_compact: first argument must be a message list".to_string(),
            ))
        }
    };
    let options = args.get(1).and_then(|v| v.as_dict()).cloned();
    let mut config = crate::orchestration::AutoCompactConfig::default();
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("compact_threshold"))
        .and_then(|v| v.as_int())
    {
        config.token_threshold = v.max(0) as usize;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("tool_output_max_chars"))
        .and_then(|v| v.as_int())
    {
        config.tool_output_max_chars = v.max(0) as usize;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("keep_last"))
        .and_then(|v| v.as_int())
    {
        config.keep_last = v.max(0) as usize;
    }
    if let Some(strategy) = options
        .as_ref()
        .and_then(|o| o.get("compact_strategy"))
        .map(|v| v.display())
    {
        config.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
    }
    if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
        config.custom_compactor = Some(callback.clone());
        if !options
            .as_ref()
            .is_some_and(|o| o.contains_key("compact_strategy"))
        {
            config.compact_strategy = crate::orchestration::CompactStrategy::Custom;
        }
    }
    let llm_opts = if config.compact_strategy == crate::orchestration::CompactStrategy::Llm {
        Some(crate::llm::extract_llm_options(&[
            VmValue::String(Rc::from("")),
            VmValue::Nil,
            args.get(1).cloned().unwrap_or(VmValue::Nil),
        ])?)
    } else {
        None
    };
    crate::orchestration::auto_compact_messages(&mut messages, &config, llm_opts.as_ref()).await?;
    Ok(VmValue::List(Rc::new(
        messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    )))
}
