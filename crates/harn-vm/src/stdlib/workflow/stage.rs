//! Single-stage execution, verification, and outcome classification.

use std::collections::VecDeque;

use serde::Deserialize;

use crate::orchestration::{
    select_workflow_stage_artifacts, ArtifactRecord, LlmUsageRecord, RunStageAttemptRecord,
    RunStageRecord,
};
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::usage::{llm_usage_delta, llm_usage_snapshot};

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
        consumed_artifact_ids: stage.consumed_artifact_ids,
    })
}

#[derive(Debug, Deserialize)]
struct WorkflowStageAttemptOutcome {
    outcome: String,
    branch: Option<String>,
    verification: serde_json::Value,
}

pub(super) async fn stage_attempt_outcome(
    ctx: &AsyncBuiltinCtx,
    node: &crate::orchestration::WorkflowNode,
    result: &serde_json::Value,
    verification: Option<serde_json::Value>,
) -> Result<(String, Option<String>, serde_json::Value), VmError> {
    let payload = serde_json::json!({
        "node": node,
        "result": result,
        "verification": verification,
    });
    let classified: WorkflowStageAttemptOutcome =
        crate::stdlib::harn_entry::call_harn_export_typed(
            ctx,
            "std/workflow/stage",
            "workflow_stage_attempt_outcome",
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
    ctx: &AsyncBuiltinCtx,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> Result<Option<StageAttemptSuccess>, VmError> {
    let payload = serde_json::json!({
        "node_id": node_id,
        "node": node,
        "artifacts": artifacts,
    });
    let planned: WorkflowStaticStagePlan = crate::stdlib::harn_entry::call_harn_export_typed(
        ctx,
        "std/workflow/stage",
        "workflow_prepare_static_stage",
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
    Ok(Some(StageAttemptSuccess {
        result,
        artifacts: planned
            .artifacts
            .into_iter()
            .map(ArtifactRecord::normalize)
            .collect(),
        transcript: None,
        outcome,
        branch: planned.branch,
        verification: planned.verification,
    }))
}

/// Everything one settled (non-erroring) stage attempt produces before the
/// retry loop decides whether to stop or go again. This is also the payload
/// shape `__host_stage_execute_once` returns across the builtin seam.
#[derive(Debug)]
pub(super) struct StageAttemptSuccess {
    pub(super) result: serde_json::Value,
    pub(super) artifacts: Vec<ArtifactRecord>,
    /// Transcript to thread into the next attempt / stage. Deliberately kept
    /// as an in-process `VmValue` (never serialized at the builtin seam):
    /// transcripts can be large and may reference session-backed message
    /// lists, so both the Rust loop and `__host_stage_execute_once` pass the
    /// value through by handle instead of round-tripping it through JSON.
    pub(super) transcript: Option<VmValue>,
    pub(super) outcome: String,
    pub(super) branch: Option<String>,
    pub(super) verification: Option<serde_json::Value>,
}

/// Artifact visibility for one stage: the context-policy / input-contract
/// selection plus the derived consumed-artifact lineage ids.
#[derive(Debug)]
pub(super) struct StageArtifactSelection {
    pub(super) selected: Vec<ArtifactRecord>,
    pub(super) consumed_artifact_ids: Vec<String>,
}

/// Select the artifacts one stage may see and derive its consumed-artifact
/// ids. Internal engine behind `__host_stage_select_artifacts`; the Rust
/// retry loop in [`execute_stage_attempts`] drives through the same function.
pub(super) async fn stage_select_artifacts(
    ctx: &AsyncBuiltinCtx,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> Result<StageArtifactSelection, VmError> {
    let selected =
        select_workflow_stage_artifacts(ctx, artifacts, &node.context_policy, &node.input_contract)
            .await?
            .artifacts;
    let consumed_artifact_ids = selected
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    Ok(StageArtifactSelection {
        selected,
        consumed_artifact_ids,
    })
}

/// Run exactly ONE stage attempt: static-stage preparation, then
/// subagent/default dispatch, then outcome classification. No retry
/// awareness lives here — the retry loop is the caller's job (today the
/// Rust loop in [`execute_stage_attempts`]; after the PR-I2 inversion,
/// `std/workflow/stage.harn`). Internal engine behind
/// `__host_stage_execute_once`.
///
/// Execution is attempt-independent today — the legacy loop re-issued the
/// unmodified task on every retry — so this takes no attempt number.
/// Errors surface as `Err` and the caller decides whether they are fatal:
/// the retry loop (and the builtin) turn them into failed-attempt data
/// rather than letting them abort the stage.
pub(super) async fn stage_execute_once(
    ctx: &AsyncBuiltinCtx,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
    selected_artifacts: &[ArtifactRecord],
    transcript: Option<&VmValue>,
) -> Result<StageAttemptSuccess, VmError> {
    if let Some(mut prepared) = prepare_static_stage(ctx, node_id, node, selected_artifacts).await?
    {
        // Static stages pass the input transcript through untouched.
        prepared.transcript = transcript.cloned();
        return Ok(prepared);
    }
    let attempt_task = task.to_string();
    match node.kind.as_str() {
        "subagent" => {
            let (result, produced, next_transcript) =
                super::super::agents_workers::execute_delegated_stage(
                    ctx,
                    node_id,
                    node,
                    &attempt_task,
                    artifacts,
                    transcript.cloned(),
                )
                .await?;
            let (outcome, branch, verification) = stage_attempt_outcome(
                ctx,
                node,
                &result,
                Some(serde_json::json!({"kind": "none", "ok": true})),
            )
            .await?;
            Ok(StageAttemptSuccess {
                result,
                artifacts: produced,
                transcript: next_transcript,
                outcome,
                branch,
                verification: Some(verification),
            })
        }
        _ => {
            let (result, produced, next_transcript) = crate::orchestration::execute_stage_node(
                ctx,
                node_id,
                node,
                &attempt_task,
                artifacts,
            )
            .await?;
            let (outcome, branch, verification) =
                stage_attempt_outcome(ctx, node, &result, None).await?;
            Ok(StageAttemptSuccess {
                result,
                artifacts: produced,
                transcript: next_transcript,
                outcome,
                branch,
                verification: Some(verification),
            })
        }
    }
}

/// Append one attempt record to a stage's attempt history, enforcing the
/// append-only invariant (attempt numbers stay dense and monotonic). Rust is
/// the sole author of `RunStageAttemptRecord`s: the builtin seam
/// (`__host_stage_record_attempt`) re-canonicalizes whatever crosses the
/// boundary through this typed struct before it can reach a run record.
pub(super) fn stage_record_attempt(
    attempts: &mut Vec<RunStageAttemptRecord>,
    record: RunStageAttemptRecord,
) -> Result<(), VmError> {
    let expected = attempts.len() + 1;
    if record.attempt != expected {
        return Err(VmError::Runtime(format!(
            "stage attempt record out of order: expected attempt {expected}, got {}",
            record.attempt
        )));
    }
    attempts.push(record);
    Ok(())
}

pub(super) async fn execute_stage_attempts(
    ctx: &AsyncBuiltinCtx,
    task: &str,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<ExecutedStage, VmError> {
    let selection = stage_select_artifacts(ctx, node, artifacts).await?;
    let mut attempts = Vec::new();
    let usage_before = llm_usage_snapshot();
    let max_attempts = node.retry_policy.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        let started_at = uuid::Uuid::now_v7().to_string();
        let execution = stage_execute_once(
            ctx,
            node_id,
            node,
            task,
            artifacts,
            &selection.selected,
            transcript.as_ref(),
        )
        .await;

        match execution {
            Ok(settled) => {
                let success = !matches!(settled.branch.as_deref(), Some("failed"));
                stage_record_attempt(
                    &mut attempts,
                    RunStageAttemptRecord {
                        attempt,
                        status: if success {
                            "completed".to_string()
                        } else {
                            "failed".to_string()
                        },
                        outcome: settled.outcome.clone(),
                        branch: settled.branch.clone(),
                        error: None,
                        verification: settled.verification.clone(),
                        started_at,
                        finished_at: Some(uuid::Uuid::now_v7().to_string()),
                    },
                )?;
                if success || attempt == max_attempts {
                    let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
                    return Ok(ExecutedStage {
                        status: if success {
                            "completed".to_string()
                        } else {
                            "failed".to_string()
                        },
                        outcome: settled.outcome,
                        branch: settled.branch,
                        result: settled.result,
                        artifacts: settled.artifacts,
                        transcript: settled.transcript,
                        verification: settled.verification,
                        usage,
                        error: if success {
                            None
                        } else {
                            Some("verification failed".to_string())
                        },
                        attempts,
                        consumed_artifact_ids: selection.consumed_artifact_ids,
                    });
                }
            }
            Err(error) => {
                let error_message = error.to_string();
                stage_record_attempt(
                    &mut attempts,
                    RunStageAttemptRecord {
                        attempt,
                        status: "failed".to_string(),
                        outcome: "error".to_string(),
                        branch: Some("error".to_string()),
                        error: Some(error_message.clone()),
                        verification: None,
                        started_at,
                        finished_at: Some(uuid::Uuid::now_v7().to_string()),
                    },
                )?;
                if attempt == max_attempts {
                    let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
                    return Ok(ExecutedStage {
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
                        consumed_artifact_ids: selection.consumed_artifact_ids,
                    });
                }
            }
        }
    }
    unreachable!("workflow stage retry loop always returns after at least one attempt")
}
