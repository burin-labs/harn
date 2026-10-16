//! Load and verify persisted run artifacts before projecting training examples.

use std::path::Path;

pub use crate::llm::tools::function_schema_from_catalog_row;
use harn_orchestration::training_example::{project_training_transcript, TrainingProjectionSource};
#[doc(inline)]
pub use harn_orchestration::training_example::{
    validate_training_example_pairing, AgentTrainingExample, TrainingExampleError,
    TrainingExampleRequest, TrainingMessage, TrainingPairingError, TrainingProvenance,
    TrainingSource, TrainingToolCall, TrainingToolFunction, TrainingUsage,
    TOOL_CALL_RECEIPT_VERSION, TOOL_RESULT_RECEIPT_VERSION, TRAINING_EXAMPLE_SCHEMA_VERSION,
};

/// Project one authoritative run into one training example.
///
/// The persisted descriptor must match the transcript and declare a terminal
/// record. Invalid run identity, changed bytes, or unpaired turns fail with a
/// structured error.
pub fn project_agent_training_example(
    request: &TrainingExampleRequest,
) -> Result<AgentTrainingExample, TrainingExampleError> {
    let run_record_path = request.run_record_path.as_path();
    let run = load_run(run_record_path)?;
    if run.evidence.execution_id.is_some() {
        crate::orchestration::validate_execution_evidence(&run.evidence).map_err(|error| {
            TrainingExampleError::new(
                "invalid_execution_evidence",
                format!("source run has invalid execution evidence: {error}"),
            )
        })?;
    }
    if let Some(expected) = request.run_id.as_deref() {
        if run.id != expected {
            return Err(TrainingExampleError::new(
                "run_id_mismatch",
                format!(
                    "{} holds run {}, not the requested {expected}",
                    run_record_path.display(),
                    run.id
                ),
            ));
        }
    }
    let transcript_path = super::verified_llm_transcript_pointer_path(&run, run_record_path)
        .map_err(|error| TrainingExampleError::new(error.kind, error.message))?;
    let descriptor = run
        .observability
        .as_ref()
        .and_then(|observability| {
            observability
                .transcript_pointers
                .iter()
                .find(|pointer| pointer.kind == "llm_jsonl")
        })
        .and_then(|pointer| pointer.descriptor.clone())
        .ok_or_else(|| {
            TrainingExampleError::new("missing_descriptor", "run has no llm_jsonl descriptor")
        })?;
    if !descriptor.complete {
        return Err(TrainingExampleError::new(
            "incomplete_source",
            format!(
                "{} has no terminal record; the run did not finalize",
                transcript_path.display()
            ),
        ));
    }
    let text = std::fs::read_to_string(&transcript_path).map_err(|error| {
        TrainingExampleError::new(
            "unavailable",
            format!("failed to read {}: {error}", transcript_path.display()),
        )
    })?;
    let source = TrainingProjectionSource {
        run_id: run.id,
        execution_id: run.evidence.execution_id,
        stage_ids: run.stages.into_iter().map(|stage| stage.id).collect(),
        recorded_session_id: descriptor.session_id,
        descriptor_schema_version: descriptor.schema_version,
        transcript_path,
        transcript_sha256: descriptor.sha256,
        transcript_byte_len: descriptor.byte_len,
        terminal_status: descriptor.terminal_status.unwrap_or_default(),
    };
    project_training_transcript(&source, &text, request.session_id.as_deref())
}

/// Read the record as persisted. Refreshing its descriptor from the current
/// sidecar would bless edited bytes before the digest comparison.
fn load_run(path: &Path) -> Result<super::RunRecord, TrainingExampleError> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        TrainingExampleError::new(
            "run_record_unreadable",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    serde_json::from_str(&content).map_err(|error| {
        TrainingExampleError::new(
            "run_record_unreadable",
            format!("failed to parse {}: {error}", path.display()),
        )
    })
}
