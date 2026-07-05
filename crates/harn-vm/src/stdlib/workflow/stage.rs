//! Single-stage execution, verification, and outcome classification.

use std::collections::VecDeque;

use serde::Deserialize;

use crate::orchestration::{
    select_workflow_stage_artifacts, ArtifactRecord, LlmUsageRecord, RunStageAttemptRecord,
    RunStageRecord,
};
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::convert::to_vm;

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

/// Serialized twin of the executed-stage payload the embedded
/// `workflow_execute_stage_attempts` loop returns. Everything except the
/// transcript round-trips through JSON; the transcript is pulled out as a raw
/// `VmValue` (it may be large or reference session-backed message lists) — the
/// same handle discipline `__host_stage_execute_once` uses across the seam.
#[derive(Debug, Deserialize)]
struct HarnExecutedStage {
    status: String,
    outcome: String,
    branch: Option<String>,
    result: serde_json::Value,
    #[serde(default)]
    artifacts: Vec<ArtifactRecord>,
    verification: Option<serde_json::Value>,
    #[serde(default)]
    usage: LlmUsageRecord,
    error: Option<String>,
    #[serde(default)]
    attempts: Vec<RunStageAttemptRecord>,
    #[serde(default)]
    consumed_artifact_ids: Vec<String>,
}

/// Encode a node for the embedded stage loop, re-attaching the raw VmValue
/// fields serde drops (`#[serde(skip)]`) so they survive the crossing and can
/// be re-lifted by `__host_stage_execute_once` via `parse_workflow_node_value`.
/// Without this the inverted loop would silently lose closure-carrying tools /
/// model policies and the session-id that surfaces a stage transcript — fields
/// the pre-inversion Rust loop passed through by reference.
fn node_to_vm_with_raw(node: &crate::orchestration::WorkflowNode) -> Result<VmValue, VmError> {
    let encoded = to_vm(node)?;
    let VmValue::Dict(dict) = encoded else {
        return Ok(encoded);
    };
    let mut dict = (*dict).clone();
    for (key, raw) in [
        ("tools", &node.raw_tools),
        ("model_policy", &node.raw_model_policy),
        ("context_assembler", &node.raw_context_assembler),
        ("auto_compact", &node.raw_auto_compact),
        // fn-verify: re-attach the live verifier closure so the embedded stage
        // loop can invoke it against each attempt's result.
        ("verify", &node.raw_verify),
    ] {
        if let Some(value) = raw {
            dict.insert(crate::value::intern_key(key), value.clone());
        }
    }
    Ok(VmValue::dict(dict))
}

/// Build the raw retry-policy dict handed to the embedded stage loop. The
/// `repair_prompt_builder` closure cannot cross serde, so it travels here as a
/// raw `VmValue` alongside the typed `feedback`/`max_attempts` fields.
fn retry_policy_to_vm(policy: &crate::orchestration::RetryPolicy) -> Result<VmValue, VmError> {
    let mut dict = crate::value::DictMap::new();
    dict.insert(
        crate::value::intern_key("max_attempts"),
        VmValue::Int(policy.max_attempts.max(1) as i64),
    );
    if let Some(feedback) = &policy.feedback {
        dict.insert(crate::value::intern_key("feedback"), to_vm(feedback)?);
    }
    if let Some(builder) = &policy.repair_prompt_builder {
        dict.insert(
            crate::value::intern_key("repair_prompt_builder"),
            builder.0.clone(),
        );
    }
    Ok(VmValue::dict(dict))
}

/// Reconstruct an [`ExecutedStage`] from the embedded loop's return value,
/// threading the transcript through as a raw handle.
fn executed_stage_from_vm(value: VmValue) -> Result<ExecutedStage, VmError> {
    let transcript = value
        .as_dict()
        .and_then(|dict| dict.get("transcript"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    let parsed: HarnExecutedStage = serde_json::from_value(crate::llm::vm_value_to_json(&value))
        .map_err(|error| {
            VmError::Runtime(format!(
                "workflow_execute_stage_attempts returned invalid shape: {error}"
            ))
        })?;
    Ok(ExecutedStage {
        status: parsed.status,
        outcome: parsed.outcome,
        branch: parsed.branch,
        result: parsed.result,
        artifacts: parsed
            .artifacts
            .into_iter()
            .map(ArtifactRecord::normalize)
            .collect(),
        transcript,
        verification: parsed.verification,
        usage: parsed.usage,
        error: parsed.error,
        attempts: parsed.attempts,
        consumed_artifact_ids: parsed.consumed_artifact_ids,
    })
}

/// Thin Rust shim over the embedded per-stage attempt/retry loop
/// (`workflow_execute_stage_attempts` in `std/workflow/stage.harn`). The loop
/// itself — attempt counting, retry-with-feedback prompt threading, stop /
/// continue — lives in Harn (design D5). Rust keeps only the enforcement /
/// attestation leaves the loop calls via builtins: capability-enforced leaf
/// execution (`__host_stage_execute_once`), append-only attempt recording
/// (`__host_stage_record_attempt`), artifact selection, and usage accounting.
///
/// Enforcement placement: the per-stage capability/effect policy is installed
/// on the thread-local execution-policy stack by `prepare_workflow_stage_state`
/// *before* this shim runs, and is read at the leaf (`execute_stage_node` /
/// `execute_delegated_stage`) on every `__host_stage_execute_once` crossing —
/// which runs on the same thread as the child VM. Moving the loop to Harn
/// therefore cannot execute anything outside the guard by construction.
pub(super) async fn execute_stage_attempts(
    ctx: &AsyncBuiltinCtx,
    task: &str,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<ExecutedStage, VmError> {
    let args = vec![
        VmValue::String(arcstr::ArcStr::from(task)),
        VmValue::String(arcstr::ArcStr::from(node_id)),
        node_to_vm_with_raw(node)?,
        to_vm(&artifacts)?,
        transcript.unwrap_or(VmValue::Nil),
        retry_policy_to_vm(&node.retry_policy)?,
    ];
    let result = crate::stdlib::harn_entry::call_harn_export_by_name(
        ctx,
        "std/workflow/stage",
        "workflow_execute_stage_attempts",
        "workflow_execute_stage_attempts",
        &args,
    )
    .await?;
    executed_stage_from_vm(result)
}
