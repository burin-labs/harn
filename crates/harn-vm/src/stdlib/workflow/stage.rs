//! Single-stage execution, verification, and outcome classification.

use std::collections::VecDeque;

use serde::Deserialize;

use crate::orchestration::{
    pop_execution_policy, push_execution_policy, select_workflow_stage_artifacts, ArtifactRecord,
    LlmUsageRecord, RunStageAttemptRecord, RunStageRecord,
};
use crate::value::{VmError, VmValue};

use super::artifact::artifact_from_value;
use super::map::{
    execute_join_tasks, map_branch_artifact, map_execution_plan, map_finalize, LocalTask,
    MapBranchResult, MapWorkItem,
};
use super::usage::{llm_usage_delta, llm_usage_snapshot, merge_usage};

#[derive(Debug)]
pub(super) struct ExecutedStage {
    pub(super) status: String,
    pub(super) outcome: String,
    pub(super) branch: Option<String>,
    pub(super) result: serde_json::Value,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) transcript: Option<VmValue>,
    pub(super) verification: Option<serde_json::Value>,
    pub(super) usage: LlmUsageRecord,
    pub(super) error: Option<String>,
    pub(super) attempts: Vec<RunStageAttemptRecord>,
    pub(super) consumed_artifact_ids: Vec<String>,
}

pub(super) fn replay_stage(
    current: &str,
    replay_stages: &mut VecDeque<RunStageRecord>,
) -> Result<ExecutedStage, VmError> {
    let Some(stage) = replay_stages.pop_front() else {
        return Err(VmError::Runtime(format!(
            "workflow replay exhausted before node {current}"
        )));
    };
    if stage.node_id != current {
        return Err(VmError::Runtime(format!(
            "workflow replay mismatch: expected node {current}, next replay stage is {}",
            stage.node_id
        )));
    }
    let mut result = serde_json::json!({
        "status": stage.status,
        "visible_text": stage.visible_text,
        "private_reasoning": stage.private_reasoning,
    });
    for key in [
        "worker",
        "prompt",
        "system_prompt",
        "rendered_context",
        "verification_contracts",
        "rendered_verification_context",
        "selected_artifact_ids",
        "selected_artifact_titles",
        "tools",
    ] {
        if let Some(value) = stage.metadata.get(key) {
            result[key] = value.clone();
        }
    }
    Ok(ExecutedStage {
        status: stage.status.clone(),
        outcome: stage.outcome.clone(),
        branch: stage.branch.clone(),
        result,
        artifacts: stage.artifacts.clone(),
        transcript: stage
            .transcript
            .as_ref()
            .map(crate::stdlib::json_to_vm_value),
        verification: stage.verification.clone(),
        usage: stage.usage.clone().unwrap_or_default(),
        error: stage
            .metadata
            .get("error")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        attempts: stage.attempts.clone(),
        consumed_artifact_ids: stage.consumed_artifact_ids.clone(),
    })
}

#[derive(Debug, Deserialize)]
struct WorkflowStageAttemptOutcome {
    outcome: String,
    branch: Option<String>,
    verification: serde_json::Value,
}

pub(super) async fn stage_attempt_outcome(
    node: &crate::orchestration::WorkflowNode,
    result: &serde_json::Value,
    verification: Option<serde_json::Value>,
) -> Result<(String, Option<String>, serde_json::Value), VmError> {
    let payload = serde_json::json!({
        "node": node,
        "result": result,
        "verification": verification,
    });
    let classified: WorkflowStageAttemptOutcome = crate::stdlib::call_harn_stdlib_typed(
        "std/workflow/stage",
        "workflow_stage_attempt_outcome",
        payload,
    )
    .await?;
    Ok((
        classified.outcome,
        classified.branch,
        classified.verification,
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WorkflowStaticStagePlan {
    handled: bool,
    result: Option<serde_json::Value>,
    artifacts: Vec<ArtifactRecord>,
    outcome: Option<String>,
    branch: Option<String>,
    verification: Option<serde_json::Value>,
}

async fn prepare_static_stage(
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> Result<Option<StageAttemptResult>, VmError> {
    let payload = serde_json::json!({
        "node_id": node_id,
        "node": node,
        "artifacts": artifacts,
    });
    let planned: WorkflowStaticStagePlan = crate::stdlib::call_harn_stdlib_typed(
        "std/workflow/stage",
        "workflow_prepare_static_stage",
        payload,
    )
    .await?;
    if !planned.handled {
        return Ok(None);
    }
    let result = planned.result.ok_or_else(|| {
        VmError::Runtime("workflow_prepare_static_stage omitted result".to_string())
    })?;
    let outcome = planned.outcome.ok_or_else(|| {
        VmError::Runtime("workflow_prepare_static_stage omitted outcome".to_string())
    })?;
    Ok(Some((
        result,
        planned
            .artifacts
            .into_iter()
            .map(ArtifactRecord::normalize)
            .collect(),
        None,
        outcome,
        planned.branch,
        planned.verification,
    )))
}

type StageAttemptResult = (
    serde_json::Value,
    Vec<ArtifactRecord>,
    Option<VmValue>,
    String,
    Option<String>,
    Option<serde_json::Value>,
);

pub(super) async fn execute_stage_attempts(
    task: &str,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<ExecutedStage, VmError> {
    let selected_stage_artifacts =
        select_workflow_stage_artifacts(artifacts, &node.context_policy, &node.input_contract)
            .await?
            .artifacts;
    let consumed_artifact_ids = selected_stage_artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    // A stage runs once. Iteration is expressed at two levels: loop-back
    // edges in the workflow graph (for cross-stage retry) and
    // `exit_when_verified` + tool feedback inside the agent loop (for
    // intra-stage iteration). `RetryPolicy` fields remain for serde
    // compatibility but are no-ops.
    let mut attempts = Vec::new();
    let started_at = uuid::Uuid::now_v7().to_string();
    let usage_before = llm_usage_snapshot();
    let attempt = 1usize;
    let attempt_task = task.to_string();
    let execution_future = async {
        if let Some((result, produced, _, outcome, branch, verification)) =
            prepare_static_stage(node_id, node, &selected_stage_artifacts).await?
        {
            return Ok((
                result,
                produced,
                transcript.clone(),
                outcome,
                branch,
                verification,
            ));
        }
        let r: Result<StageAttemptResult, VmError> = match node.kind.as_str() {
            "map" => {
                let plan = map_execution_plan(node, artifacts).await?;
                let total_items = plan.total_items;
                let join_target = plan.join_target;
                let max_concurrent = plan.max_concurrent;
                let stage_template = plan.stage_node;
                let output_kind = plan.output_kind;
                let lineage = plan.lineage;
                let strategy = plan.strategy;
                let branch_policy = crate::orchestration::current_execution_policy();
                let tasks = plan
                    .items
                    .into_iter()
                    .map(|item| {
                        let branch_policy = branch_policy.clone();
                        let branch_transcript = transcript.clone();
                        let task_label = task.to_string();
                        let stage_template = stage_template.clone();
                        let node_id = node_id.to_string();
                        let output_kind = output_kind.clone();
                        let lineage = lineage.clone();
                        Box::pin(async move {
                            if let Some(policy) = branch_policy.clone() {
                                push_execution_policy(policy);
                            }
                            let result = match stage_template {
                                Some(stage_node) => {
                                    let index = match &item {
                                        MapWorkItem::Artifact { index, .. }
                                        | MapWorkItem::Value { index, .. } => *index,
                                    };
                                    let branch_input =
                                        vec![map_branch_artifact(&node_id, &item, &lineage)];
                                    let branch_task = format!(
                                        "{task_label}\n\nMap item {} of {}",
                                        index + 1,
                                        total_items.max(1)
                                    );
                                    let executed = execute_stage_attempts(
                                        &branch_task,
                                        &format!("{node_id}_map_{}", index + 1),
                                        &stage_node,
                                        &branch_input,
                                        branch_transcript,
                                    )
                                    .await?;
                                    Ok(MapBranchResult {
                                        index,
                                        status: executed.status.clone(),
                                        result: executed.result,
                                        artifacts: executed.artifacts,
                                        usage: executed.usage,
                                        error: executed.error,
                                    })
                                }
                                None => {
                                    let index = match &item {
                                        MapWorkItem::Artifact { index, .. }
                                        | MapWorkItem::Value { index, .. } => *index,
                                    };
                                    let artifact = match &item {
                                        MapWorkItem::Artifact { artifact, .. } => {
                                            let value = artifact
                                                .data
                                                .clone()
                                                .or_else(|| {
                                                    artifact
                                                        .text
                                                        .clone()
                                                        .map(serde_json::Value::String)
                                                })
                                                .unwrap_or(serde_json::Value::Null);
                                            artifact_from_value(
                                                &node_id,
                                                &output_kind,
                                                index,
                                                value,
                                                std::slice::from_ref(&artifact.id),
                                                format!("map {} item {}", node_id, index + 1),
                                            )
                                        }
                                        MapWorkItem::Value { value, .. } => artifact_from_value(
                                            &node_id,
                                            &output_kind,
                                            index,
                                            value.clone(),
                                            &lineage,
                                            format!("map {} item {}", node_id, index + 1),
                                        ),
                                    };
                                    Ok(MapBranchResult {
                                        index,
                                        status: "completed".to_string(),
                                        result: serde_json::json!({
                                            "status": "completed",
                                            "text": artifact.text,
                                        }),
                                        artifacts: vec![artifact],
                                        usage: LlmUsageRecord::default(),
                                        error: None,
                                    })
                                }
                            };
                            if branch_policy.is_some() {
                                pop_execution_policy();
                            }
                            result
                        }) as LocalTask<Result<MapBranchResult, VmError>>
                    })
                    .collect::<Vec<_>>();

                let branch_results = execute_join_tasks(tasks, join_target, max_concurrent).await;

                let mut completed = Vec::new();
                let mut failures = Vec::new();
                let mut produced = Vec::new();
                let mut usage = LlmUsageRecord::default();
                for branch_result in branch_results {
                    match branch_result {
                        Ok(Ok(branch)) => {
                            merge_usage(&mut usage, &branch.usage);
                            if branch.status == "completed" && branch.error.is_none() {
                                produced.extend(branch.artifacts.clone());
                                completed.push(serde_json::json!({
                                    "index": branch.index,
                                    "status": branch.status,
                                    "result": branch.result,
                                    "artifact_count": branch.artifacts.len(),
                                }));
                            } else {
                                failures.push(serde_json::json!({
                                    "index": branch.index,
                                    "status": branch.status,
                                    "error": branch.error,
                                }));
                            }
                        }
                        Ok(Err(error)) => failures.push(serde_json::json!({
                            "status": "failed",
                            "error": error.to_string(),
                        })),
                        Err(error) => failures.push(serde_json::json!({
                            "status": "failed",
                            "error": error,
                        })),
                    }
                }
                produced.sort_by(|left, right| {
                    let left_index = left
                        .metadata
                        .get("index")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(u64::MAX);
                    let right_index = right
                        .metadata
                        .get("index")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(u64::MAX);
                    left_index.cmp(&right_index)
                });
                let (result, outcome, branch) =
                    map_finalize(&strategy, total_items, produced.len(), completed, failures)
                        .await?;
                Ok((result, produced, transcript.clone(), outcome, branch, None))
            }
            "subagent" => {
                let (result, produced, next_transcript) =
                    super::super::agents_workers::execute_delegated_stage(
                        node_id,
                        node,
                        &attempt_task,
                        artifacts,
                        transcript.clone(),
                    )
                    .await?;
                let (outcome, branch, verification) = stage_attempt_outcome(
                    node,
                    &result,
                    Some(serde_json::json!({"kind": "none", "ok": true})),
                )
                .await?;
                Ok((
                    result,
                    produced,
                    next_transcript,
                    outcome,
                    branch,
                    Some(verification),
                ))
            }
            _ => {
                let (result, produced, next_transcript) = crate::orchestration::execute_stage_node(
                    node_id,
                    node,
                    &attempt_task,
                    artifacts,
                )
                .await?;
                let (outcome, branch, verification) =
                    stage_attempt_outcome(node, &result, None).await?;
                Ok((
                    result,
                    produced,
                    next_transcript,
                    outcome,
                    branch,
                    Some(verification),
                ))
            }
        };
        r
    };
    let execution: Result<StageAttemptResult, VmError> = execution_future.await;

    match execution {
        Ok((result, produced, next_transcript, outcome, branch, verification)) => {
            let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
            let success = !matches!(branch.as_deref(), Some("failed"));
            attempts.push(RunStageAttemptRecord {
                attempt,
                status: if success {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                },
                outcome: outcome.clone(),
                branch: branch.clone(),
                error: None,
                verification: verification.clone(),
                started_at,
                finished_at: Some(uuid::Uuid::now_v7().to_string()),
            });
            Ok(ExecutedStage {
                status: if success {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                },
                outcome,
                branch,
                result,
                artifacts: produced,
                transcript: next_transcript,
                verification,
                usage,
                error: if success {
                    None
                } else {
                    Some("verification failed".to_string())
                },
                attempts,
                consumed_artifact_ids,
            })
        }
        Err(error) => {
            let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
            let error_message = error.to_string();
            attempts.push(RunStageAttemptRecord {
                attempt,
                status: "failed".to_string(),
                outcome: "error".to_string(),
                branch: Some("error".to_string()),
                error: Some(error_message.clone()),
                verification: None,
                started_at,
                finished_at: Some(uuid::Uuid::now_v7().to_string()),
            });
            Ok(ExecutedStage {
                status: "failed".to_string(),
                outcome: "error".to_string(),
                branch: Some("error".to_string()),
                result: serde_json::json!({"status": "failed", "text": ""}),
                artifacts: Vec::new(),
                transcript: transcript.clone(),
                verification: None,
                usage,
                error: Some(error_message),
                attempts,
                consumed_artifact_ids,
            })
        }
    }
}
