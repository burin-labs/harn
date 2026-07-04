//! Host workflow registration-layer builtins for the Harn stdlib executor.

use std::collections::BTreeMap;

use crate::orchestration::{
    normalize_workflow_value, ArtifactRecord, RunStageAttemptRecord, WorkflowEdge,
};
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::super::parse_artifact_list;

// Builtin-name string constants — referenced inside fn bodies for
// error formatting. Only the ones that appear in error paths remain.
const HOST_WORKFLOW_MAP_PLAN_BUILTIN: &str = "__host_workflow_map_plan";
const HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN: &str = "__host_workflow_map_branch_artifact";
const HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN: &str = "__host_workflow_map_execute_branch";
const HOST_WORKFLOW_MAP_FINALIZE_BUILTIN: &str = "__host_workflow_map_finalize";
const HOST_STAGE_SELECT_ARTIFACTS_BUILTIN: &str = "__host_stage_select_artifacts";
const HOST_STAGE_EXECUTE_ONCE_BUILTIN: &str = "__host_stage_execute_once";
const HOST_STAGE_RECORD_ATTEMPT_BUILTIN: &str = "__host_stage_record_attempt";
const HOST_LLM_USAGE_DELTA_BUILTIN: &str = "__host_llm_usage_delta";

use super::artifact::artifact_from_value;
use super::convert::to_vm;
use super::map::{
    map_branch_artifact, map_execution_plan, map_finalize, MapBranchResult, MapExecutionPlan,
    MapWorkItem,
};
use super::stage::{
    execute_stage_attempts, stage_execute_once, stage_record_attempt, stage_select_artifacts,
};
use super::state::{
    complete_workflow_stage_state, finalize_workflow_state, insert_workflow_state, map_item_index,
    parse_executed_stage_record, parse_json_arg, parse_options_arg, parse_state_id_arg,
    parse_string_list_arg, prepare_workflow_stage_state, prepare_workflow_state,
    record_workflow_transitions, remove_workflow_state, string_list_to_vm, workflow_control_to_vm,
};
use super::usage::{llm_usage_delta, llm_usage_snapshot, UsageSnapshot};

/// Prepare low-level workflow run state for the Harn stdlib workflow executor.
#[harn_builtin(
    sig = "__host_workflow_prepare_run(task: string, graph: dict, artifacts?: list|nil, options?: dict|nil) -> dict",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_workflow_prepare_run_builtin(
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

/// Prepare one low-level workflow stage and install its execution scope.
#[harn_builtin(
    sig = "__host_workflow_stage_prepare(state_id: string, node_id: string, ready_nodes: list, options?: dict|nil) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_workflow_stage_prepare_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let state_id = parse_state_id_arg(args.first(), "__host_workflow_stage_prepare")?;
    let mut state = remove_workflow_state(&state_id, "__host_workflow_stage_prepare")?;
    let node_id = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("__host_workflow_stage_prepare: missing node id".to_string())
        })?;
    state.ready_nodes =
        parse_string_list_arg(args.get(2), "__host_workflow_stage_prepare ready_nodes")?;
    let options = parse_options_arg(&args, 3);
    let (state, plan) = prepare_workflow_stage_state(&ctx, state, node_id, &options).await?;
    insert_workflow_state(state);
    Ok(plan)
}

/// Complete one prepared low-level workflow stage and tear down its execution scope.
#[harn_builtin(
    sig = "__host_workflow_stage_complete(state_id: string, node_id: string, llm_result: any) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_workflow_stage_complete_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let state_id = parse_state_id_arg(args.first(), "__host_workflow_stage_complete")?;
    let state = remove_workflow_state(&state_id, "__host_workflow_stage_complete")?;
    let node_id = args
        .get(1)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("__host_workflow_stage_complete: missing node id".to_string())
        })?;
    let llm_result = crate::llm::vm_value_to_json(args.get(2).unwrap_or(&VmValue::Nil));
    let (state, stage) = complete_workflow_stage_state(&ctx, state, node_id, llm_result).await?;
    let branch = stage.branch.clone();
    let control = workflow_control_to_vm(&state, false)?;
    insert_workflow_state(state);
    let mut dict = BTreeMap::new();
    dict.insert("state".to_string(), control);
    dict.insert("stage".to_string(), to_vm(&stage)?);
    dict.insert(
        "branch".to_string(),
        branch
            .map(|branch| VmValue::String(arcstr::ArcStr::from(branch)))
            .unwrap_or(VmValue::Nil),
    );
    Ok(VmValue::dict(dict))
}

/// Record workflow stage transitions and checkpoint low-level run state.
#[harn_builtin(
    sig = "__host_workflow_record_transitions(state_id: string, ready_nodes: list, stage: dict, edges: list) -> dict",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_workflow_record_transitions_builtin(
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

/// Finalize low-level workflow run state and persist the final checkpoint.
#[harn_builtin(
    sig = "__host_workflow_finalize_run(state_id: string, ready_nodes: list) -> dict",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_workflow_finalize_run_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let state_id = parse_state_id_arg(args.first(), "__host_workflow_finalize_run")?;
    let mut state = remove_workflow_state(&state_id, "__host_workflow_finalize_run")?;
    state.ready_nodes =
        parse_string_list_arg(args.get(1), "__host_workflow_finalize_run ready_nodes")?;
    finalize_workflow_state(state)
}

/// Return the host-normalized execution plan for a workflow map stage.
#[harn_builtin(
    sig = "__host_workflow_map_plan(node: dict, artifacts: list) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_workflow_map_plan_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let node: crate::orchestration::WorkflowNode =
        parse_json_arg(args.first(), HOST_WORKFLOW_MAP_PLAN_BUILTIN)?;
    let artifacts = parse_artifact_list(args.get(1))?;
    let plan = map_execution_plan(&ctx, &node, &artifacts).await?;
    to_vm(&plan)
}

/// Build the synthesized input artifact for one Harn-owned workflow map branch.
#[harn_builtin(
    sig = "__host_workflow_map_branch_artifact(node_id: string, item: any, lineage: dict) -> dict",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_workflow_map_branch_artifact_builtin(
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

/// Execute one workflow map branch while Harn owns branch scheduling.
#[harn_builtin(
    sig = "__host_workflow_map_execute_branch(node_id: string, plan: dict, item: any, branch_artifact: dict, options?: dict|nil) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_workflow_map_execute_branch_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
            &ctx,
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

/// Finalize a Harn-owned workflow map stage after branch settlement.
#[harn_builtin(
    sig = "__host_workflow_map_finalize(strategy: string, total_items: int, completed: list, failures: list, produced: list) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_workflow_map_finalize_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
    let (result, outcome, branch) = map_finalize(
        &ctx,
        &strategy,
        total_items,
        produced.len(),
        completed,
        failures,
    )
    .await?;
    to_vm(&serde_json::json!({
        "result": result,
        "outcome": outcome,
        "branch": branch,
    }))
}

// --- Stage attempt-loop seams (inversion pre-work, design D5 step 1) ------
//
// The four builtins below expose the exact internal functions the Rust
// retry loop in `stage.rs::execute_stage_attempts` drives through, so the
// loop itself can move into `std/workflow/stage.harn` (PR-I2) without the
// mechanisms changing.
//
// Handle design: these seams are value-threaded, not state-store-threaded.
// The stage attempt loop also runs where no `WorkflowRunState` entry exists
// (workflow map-branch execution) or while the run state is checked out of
// the thread-local store (`__host_workflow_stage_prepare` holds it), so the
// node / artifacts / transcript travel as explicit VmValue arguments — the
// same identification scheme `__host_workflow_map_plan` and
// `__host_workflow_map_execute_branch` already use. Like those builtins,
// `node` crosses via serde, which drops `#[serde(skip)]` raw closure fields
// (`raw_tools` / `raw_model_policy` / …) — identical to the existing
// map-branch precedent.

/// Select the artifacts visible to one workflow stage and derive its
/// consumed-artifact lineage ids.
#[harn_builtin(
    sig = "__host_stage_select_artifacts(node: dict, artifacts: list) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_stage_select_artifacts_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let node: crate::orchestration::WorkflowNode =
        parse_json_arg(args.first(), HOST_STAGE_SELECT_ARTIFACTS_BUILTIN)?;
    let artifacts = parse_artifact_list(args.get(1))?;
    let selection = stage_select_artifacts(&ctx, &node, &artifacts).await?;
    let mut dict = BTreeMap::new();
    dict.insert("artifacts".to_string(), to_vm(&selection.selected)?);
    dict.insert(
        "consumed_artifact_ids".to_string(),
        string_list_to_vm(&selection.consumed_artifact_ids),
    );
    Ok(VmValue::dict(dict))
}

/// Execute exactly ONE workflow stage attempt (static-stage preparation,
/// subagent/default dispatch, outcome classification) and return the settled
/// attempt payload.
///
/// Returns `{ok: true, result, artifacts, transcript, outcome, branch,
/// verification}` on a settled attempt, or `{ok: false, error, outcome:
/// "error", branch: "error"}` when execution raised. The legacy Rust retry
/// loop caught `VmError` from an attempt and recorded it as failed-attempt
/// data instead of aborting the stage, so this builtin mirrors that Ok/Err
/// discrimination in its return value rather than propagating the error to
/// the caller.
///
/// `attempt` is accepted (and validated) so the seam does not have to change
/// when retry-aware prompting lands, but execution is attempt-independent
/// today: the legacy loop re-issued the unmodified task on every retry.
///
/// `transcript` crosses in-process as an opaque VmValue in both directions
/// (never serialized to JSON): transcripts can be large and may reference
/// session-backed message lists.
#[harn_builtin(
    sig = "__host_stage_execute_once(node_id: string, node: dict, task: string, attempt: int, artifacts: list, selected_artifacts: list, transcript?: any) -> dict",
    kind = "async",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) async fn host_stage_execute_once_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let node_id = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_STAGE_EXECUTE_ONCE_BUILTIN}: missing node id"
            ))
        })?;
    // Parse via `parse_workflow_node_value` (not `parse_json_arg`) so the raw
    // VmValue fields serde drops — closure-carrying `tools` / `model_policy` /
    // `context_assembler` and the session-id that surfaces a stage transcript —
    // are re-lifted from the dict the shim re-attached them to. This keeps the
    // inverted stage loop fidelity-equal to the pre-inversion Rust path.
    let node = crate::orchestration::parse_workflow_node_value(
        args.get(1).ok_or_else(|| {
            VmError::Runtime(format!("{HOST_STAGE_EXECUTE_ONCE_BUILTIN}: missing node"))
        })?,
        HOST_STAGE_EXECUTE_ONCE_BUILTIN,
    )?;
    let task = args.get(2).map(|value| value.display()).unwrap_or_default();
    match args.get(3) {
        None | Some(VmValue::Nil) => {}
        Some(VmValue::Int(value)) if *value >= 1 => {}
        Some(VmValue::Int(value)) => {
            return Err(VmError::Runtime(format!(
                "{HOST_STAGE_EXECUTE_ONCE_BUILTIN}: attempt must be >= 1, got {value}"
            )))
        }
        Some(value) => {
            return Err(VmError::Runtime(format!(
                "{HOST_STAGE_EXECUTE_ONCE_BUILTIN}: attempt must be an int, got {}",
                value.type_name()
            )))
        }
    }
    let artifacts = parse_artifact_list(args.get(4))?;
    let selected_artifacts = parse_artifact_list(args.get(5))?;
    let transcript = args
        .get(6)
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    match stage_execute_once(
        &ctx,
        &node_id,
        &node,
        &task,
        &artifacts,
        &selected_artifacts,
        transcript.as_ref(),
    )
    .await
    {
        Ok(settled) => {
            let mut dict = BTreeMap::new();
            dict.insert("ok".to_string(), VmValue::Bool(true));
            dict.insert(
                "result".to_string(),
                crate::stdlib::json_to_vm_value(&settled.result),
            );
            dict.insert("artifacts".to_string(), to_vm(&settled.artifacts)?);
            dict.insert(
                "transcript".to_string(),
                settled.transcript.unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "outcome".to_string(),
                VmValue::String(arcstr::ArcStr::from(settled.outcome)),
            );
            dict.insert(
                "branch".to_string(),
                settled
                    .branch
                    .map(|branch| VmValue::String(arcstr::ArcStr::from(branch)))
                    .unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "verification".to_string(),
                settled
                    .verification
                    .as_ref()
                    .map(crate::stdlib::json_to_vm_value)
                    .unwrap_or(VmValue::Nil),
            );
            Ok(VmValue::dict(dict))
        }
        Err(error) => {
            let mut dict = BTreeMap::new();
            dict.insert("ok".to_string(), VmValue::Bool(false));
            dict.insert(
                "error".to_string(),
                VmValue::String(arcstr::ArcStr::from(error.to_string())),
            );
            dict.insert(
                "outcome".to_string(),
                VmValue::String(arcstr::ArcStr::from("error")),
            );
            dict.insert(
                "branch".to_string(),
                VmValue::String(arcstr::ArcStr::from("error")),
            );
            Ok(VmValue::dict(dict))
        }
    }
}

/// Validate and append one stage attempt record to an attempt history,
/// returning the extended history.
///
/// Append-only: `record.attempt` must be exactly `len(attempts) + 1`. Rust
/// stays the sole writer of `RunStageAttemptRecord`s — the record crosses the
/// seam as raw fields and is re-canonicalized through the typed struct here
/// before it can reach a persisted run record.
#[harn_builtin(
    sig = "__host_stage_record_attempt(attempts: list, record: dict) -> list",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_stage_record_attempt_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let mut attempts: Vec<RunStageAttemptRecord> =
        parse_json_arg(args.first(), HOST_STAGE_RECORD_ATTEMPT_BUILTIN)?;
    let record: RunStageAttemptRecord =
        parse_json_arg(args.get(1), HOST_STAGE_RECORD_ATTEMPT_BUILTIN)?;
    stage_record_attempt(&mut attempts, record)?;
    to_vm(&attempts)
}

/// Snapshot the cumulative LLM usage counters used for per-stage usage
/// accounting (tokens, duration, call count, cost, trace length).
#[harn_builtin(
    sig = "__host_llm_usage_snapshot() -> dict",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_llm_usage_snapshot_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    to_vm(&llm_usage_snapshot())
}

/// Compute the LLM usage delta between two `__host_llm_usage_snapshot`
/// snapshots. `after` defaults to the current counters when omitted, so a
/// stage-usage delta costs a single crossing.
#[harn_builtin(
    sig = "__host_llm_usage_delta(before: dict, after?: dict|nil) -> dict",
    category = "workflow.host",
    runtime_only = true
)]
pub(super) fn host_llm_usage_delta_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let before: UsageSnapshot = parse_json_arg(args.first(), HOST_LLM_USAGE_DELTA_BUILTIN)?;
    let after: UsageSnapshot = match args.get(1) {
        None | Some(VmValue::Nil) => llm_usage_snapshot(),
        Some(value) => parse_json_arg(Some(value), HOST_LLM_USAGE_DELTA_BUILTIN)?,
    };
    to_vm(&llm_usage_delta(&before, &after))
}
