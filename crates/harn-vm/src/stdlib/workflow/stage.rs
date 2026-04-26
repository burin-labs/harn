//! Single-stage execution, verification, and outcome classification.

use std::collections::VecDeque;

use crate::orchestration::{
    pop_execution_policy, push_execution_policy, select_artifacts, ArtifactRecord, LlmUsageRecord,
    RunStageAttemptRecord, RunStageRecord,
};
use crate::value::{VmError, VmValue};

use super::artifact::artifact_from_value;
use super::map::{
    execute_join_policy, map_branch_artifact, map_executes_stage, map_stage_node, map_work_items,
    LocalTask, MapBranchResult, MapWorkItem,
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

pub(super) fn evaluate_verification(
    node: &crate::orchestration::WorkflowNode,
    result: &serde_json::Value,
) -> serde_json::Value {
    let visible_text = result
        .get("visible_text")
        .or_else(|| result.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let exit_status = result
        .get("exit_status")
        .or_else(|| result.get("status_code"))
        .or_else(|| result.get("status"))
        .and_then(|value| value.as_i64());
    let Some(verify) = node.verify.as_ref().and_then(|verify| verify.as_object()) else {
        return serde_json::json!({"kind": "none", "ok": true});
    };

    let mut checks = Vec::new();
    if let Some(needle) = verify.get("assert_text").and_then(|value| value.as_str()) {
        checks.push(serde_json::json!({
            "kind": "assert_text",
            "ok": visible_text.contains(needle),
            "expected": needle,
        }));
    }
    if let Some(expected_status) = verify.get("expect_status").and_then(|value| value.as_i64()) {
        checks.push(serde_json::json!({
            "kind": "expect_status",
            "ok": exit_status == Some(expected_status),
            "expected": expected_status,
            "actual": exit_status,
        }));
    }
    if checks.is_empty() {
        return serde_json::json!({"kind": "none", "ok": true});
    }

    let ok = checks.iter().all(|check| {
        check
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    });
    serde_json::json!({
        "kind": "composite",
        "ok": ok,
        "checks": checks,
    })
}

pub(super) fn evaluate_condition(
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> (bool, Vec<String>) {
    let consumed = artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect();
    if let Some(needle) = node
        .metadata
        .get("contains_text")
        .and_then(|value| value.as_str())
    {
        let matched = artifacts.iter().any(|artifact| {
            artifact
                .text
                .as_ref()
                .is_some_and(|text| text.contains(needle))
        });
        return (matched, consumed);
    }
    if let Some(expected) = node
        .metadata
        .get("artifact_kind")
        .and_then(|value| value.as_str())
    {
        let matched = artifacts.iter().any(|artifact| artifact.kind == expected);
        return (matched, consumed);
    }
    (!artifacts.is_empty(), consumed)
}

pub(super) fn classify_stage_outcome(
    node_kind: &str,
    result: &serde_json::Value,
    verification: &serde_json::Value,
) -> (String, Option<String>) {
    let verified_ok = verification
        .get("ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let result_status = result
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("completed");
    let stage_succeeded = if node_kind == "verify" {
        verified_ok
    } else {
        result_status == "done" || result_status == "completed"
    };

    let outcome = if node_kind == "verify" {
        if verified_ok {
            "verified".to_string()
        } else {
            "verification_failed".to_string()
        }
    } else if !stage_succeeded {
        result_status.to_string()
    } else if node_kind == "subagent" {
        "subagent_completed".to_string()
    } else {
        "success".to_string()
    };

    let branch = if node_kind == "verify" {
        Some(if verified_ok {
            "passed".to_string()
        } else {
            "failed".to_string()
        })
    } else if stage_succeeded {
        Some("success".to_string())
    } else {
        Some("failed".to_string())
    };

    (outcome, branch)
}

pub(super) async fn execute_stage_attempts(
    task: &str,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<ExecutedStage, VmError> {
    type StageAttemptResult = (
        serde_json::Value,
        Vec<ArtifactRecord>,
        Option<VmValue>,
        String,
        Option<String>,
        Option<serde_json::Value>,
    );
    let consumed_artifact_ids = select_artifacts(artifacts.to_vec(), &node.context_policy)
        .into_iter()
        .map(|artifact| artifact.id)
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
        let r: Result<StageAttemptResult, VmError> = match node.kind.as_str() {
            "fork" => Ok((
                serde_json::json!({"status": "completed", "text": "forked"}),
                Vec::new(),
                transcript.clone(),
                "forked".to_string(),
                Some("fork".to_string()),
                None,
            )),
            "join" => Ok((
                serde_json::json!({"status": "completed", "text": "joined"}),
                Vec::new(),
                transcript.clone(),
                "joined".to_string(),
                Some("join".to_string()),
                None,
            )),
            "condition" => {
                let selected = select_artifacts(artifacts.to_vec(), &node.context_policy);
                let (matched, _) = evaluate_condition(node, &selected);
                Ok((
                    serde_json::json!({"status": "completed", "text": if matched { "true" } else { "false" }}),
                    Vec::new(),
                    transcript.clone(),
                    if matched {
                        "condition_true".to_string()
                    } else {
                        "condition_false".to_string()
                    },
                    Some(if matched {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    }),
                    None,
                ))
            }
            "map" => {
                let items = map_work_items(node, artifacts);
                let total_items = items.len();
                let branch_policy = crate::orchestration::current_execution_policy();
                let runs_stage = map_executes_stage(node);
                let stage_template = runs_stage.then(|| map_stage_node(node));
                let shared_lineage = items
                    .iter()
                    .flat_map(|item| match item {
                        MapWorkItem::Artifact { artifact, .. } => vec![artifact.id.clone()],
                        MapWorkItem::Value { .. } => Vec::new(),
                    })
                    .collect::<Vec<_>>();
                let strategy = if node.join_policy.strategy.is_empty() {
                    "all".to_string()
                } else {
                    node.join_policy.strategy.clone()
                };
                let tasks = items
                    .into_iter()
                    .map(|item| {
                        let branch_policy = branch_policy.clone();
                        let branch_transcript = transcript.clone();
                        let task_label = task.to_string();
                        let stage_template = stage_template.clone();
                        let node_id = node_id.to_string();
                        let output_kind = node
                            .map_policy
                            .output_kind
                            .clone()
                            .or_else(|| node.output_contract.output_kinds.first().cloned())
                            .unwrap_or_else(|| "artifact".to_string());
                        let lineage = shared_lineage.clone();
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

                let branch_results = execute_join_policy(
                    tasks,
                    &strategy,
                    node.join_policy.min_completed,
                    node.map_policy.max_concurrent,
                )
                .await;

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
                let status = if failures.is_empty() {
                    "completed"
                } else if produced.is_empty() {
                    "failed"
                } else {
                    "partial"
                };
                let text = if status == "failed" {
                    format!("map failed after {} branch failures", failures.len())
                } else {
                    format!("mapped {} of {} items", produced.len(), total_items)
                };
                let branch = if status == "failed" {
                    Some("failed".to_string())
                } else {
                    Some("mapped".to_string())
                };
                let result = serde_json::json!({
                    "status": status,
                    "text": text,
                    "join_strategy": strategy,
                    "completed": completed,
                    "failures": failures,
                });
                Ok((
                    result,
                    produced,
                    transcript.clone(),
                    "mapped".to_string(),
                    branch,
                    None,
                ))
            }
            "reduce" => {
                let selected = select_artifacts(artifacts.to_vec(), &node.context_policy);
                let separator = node
                    .reduce_policy
                    .separator
                    .clone()
                    .unwrap_or_else(|| "\n\n".to_string());
                let reduced_text = selected
                    .iter()
                    .filter_map(|artifact| artifact.text.clone())
                    .collect::<Vec<_>>()
                    .join(&separator);
                let reduced = artifact_from_value(
                    node_id,
                    node.output_contract
                        .output_kinds
                        .first()
                        .map(|kind| kind.as_str())
                        .unwrap_or("summary"),
                    0,
                    serde_json::Value::String(reduced_text.clone()),
                    &selected
                        .iter()
                        .map(|artifact| artifact.id.clone())
                        .collect::<Vec<_>>(),
                    format!("reduce {} output", node_id),
                );
                Ok((
                    serde_json::json!({"status": "completed", "text": reduced_text}),
                    vec![reduced],
                    transcript.clone(),
                    "reduced".to_string(),
                    Some("reduced".to_string()),
                    None,
                ))
            }
            "escalation" => {
                let reason = node
                    .escalation_policy
                    .reason
                    .clone()
                    .unwrap_or_else(|| "manual review required".to_string());
                let produced = artifact_from_value(
                    node_id,
                    node.output_contract
                        .output_kinds
                        .first()
                        .map(|kind| kind.as_str())
                        .unwrap_or("plan"),
                    0,
                    serde_json::json!({
                        "queue": node.escalation_policy.queue,
                        "level": node.escalation_policy.level,
                        "reason": reason,
                    }),
                    &consumed_artifact_ids,
                    format!("escalation {}", node_id),
                );
                Ok((
                    serde_json::json!({"status": "completed", "text": reason}),
                    vec![produced],
                    transcript.clone(),
                    "escalated".to_string(),
                    Some("escalated".to_string()),
                    None,
                ))
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
                Ok((
                    result,
                    produced,
                    next_transcript,
                    "subagent_completed".to_string(),
                    Some("success".to_string()),
                    None,
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
                let verification = evaluate_verification(node, &result);
                let (outcome, branch) = classify_stage_outcome(&node.kind, &result, &verification);
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
