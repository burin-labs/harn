//! Host workflow registration-layer builtins for the Harn stdlib executor.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::orchestration::{normalize_workflow_value, ArtifactRecord, WorkflowEdge};
use crate::value::{VmError, VmValue};

use super::super::parse_artifact_list;

use super::artifact::artifact_from_value;
use super::convert::to_vm;
use super::map::{
    map_branch_artifact, map_execution_plan, map_finalize, MapBranchResult, MapExecutionPlan,
    MapWorkItem,
};
use super::stage::execute_stage_attempts;
use super::state::{
    complete_workflow_stage_state, finalize_workflow_state, insert_workflow_state, map_item_index,
    parse_executed_stage_record, parse_json_arg, parse_options_arg, parse_state_id_arg,
    parse_string_list_arg, prepare_workflow_stage_state, prepare_workflow_state,
    record_workflow_transitions, remove_workflow_state, workflow_control_to_vm,
};

pub(super) const HOST_WORKFLOW_PREPARE_RUN_BUILTIN: &str = "__host_workflow_prepare_run";
pub(super) const HOST_WORKFLOW_STAGE_PREPARE_BUILTIN: &str = "__host_workflow_stage_prepare";
pub(super) const HOST_WORKFLOW_STAGE_COMPLETE_BUILTIN: &str = "__host_workflow_stage_complete";
pub(super) const HOST_WORKFLOW_RECORD_TRANSITIONS_BUILTIN: &str =
    "__host_workflow_record_transitions";
pub(super) const HOST_WORKFLOW_FINALIZE_RUN_BUILTIN: &str = "__host_workflow_finalize_run";
pub(super) const HOST_WORKFLOW_MAP_PLAN_BUILTIN: &str = "__host_workflow_map_plan";
pub(super) const HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN: &str =
    "__host_workflow_map_branch_artifact";
pub(super) const HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN: &str =
    "__host_workflow_map_execute_branch";
pub(super) const HOST_WORKFLOW_MAP_FINALIZE_BUILTIN: &str = "__host_workflow_map_finalize";

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

pub(super) async fn host_workflow_stage_prepare_builtin(
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
    let (state, plan) = prepare_workflow_stage_state(state, node_id, &options).await?;
    insert_workflow_state(state);
    Ok(plan)
}

pub(super) async fn host_workflow_stage_complete_builtin(
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
    let (state, stage) = complete_workflow_stage_state(state, node_id, llm_result).await?;
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

pub(super) async fn host_workflow_map_plan_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let node: crate::orchestration::WorkflowNode =
        parse_json_arg(args.first(), HOST_WORKFLOW_MAP_PLAN_BUILTIN)?;
    let artifacts = parse_artifact_list(args.get(1))?;
    let plan = map_execution_plan(&node, &artifacts).await?;
    to_vm(&plan)
}

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

pub(super) async fn host_workflow_map_execute_branch_builtin(
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

pub(super) async fn host_workflow_map_finalize_builtin(
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
    let (result, outcome, branch) =
        map_finalize(&strategy, total_items, produced.len(), completed, failures).await?;
    to_vm(&serde_json::json!({
        "result": result,
        "outcome": outcome,
        "branch": branch,
    }))
}
