//! Project one authoritative Harn agent run into one lossless training example.
//!
//! # Why this exists
//!
//! Downstream corpus builders used to parse the LLM transcript sidecar
//! themselves: they duplicated Harn's event vocabulary, guessed run boundaries
//! from `iteration == 0`, and silently skipped rows they could not read. That
//! is Harn-owned transcript semantics, and it drifts every time lifecycle or
//! tool-call recording changes.
//!
//! This module is the single authoritative projection. It starts from the
//! typed [`RunRecord`](crate::orchestration::RunRecord) and the verified
//! transcript descriptor written by run persistence, replays the canonical
//! event stream, and emits [`AgentTrainingExample`] — the
//! `harn.agent_training_example.v1` contract.
//!
//! # What it is not
//!
//! It is not `std/agent/transcript`. That module is deliberately a
//! compatibility/analysis normalizer: it infers iterations, defaults missing
//! ids and names, regex-classifies tool-result errors, and stays lenient
//! unless a caller opts into strict mode. Those forensic semantics are lossy,
//! so they cannot be the authority for supervised fine-tuning.
//!
//! Every source fact here is either present and typed, or the projection
//! fails with a structured [`TrainingExampleError`]. There is no lenient mode
//! and no fallback to the analysis normalizer.
//!
//! # What it does not decide
//!
//! Eligibility. A caller chooses which explicit run should become a training
//! example; Harn then projects that run faithfully or explains exactly why it
//! cannot. Product verdicts stay out of the projector.

mod project;
mod source;
mod validate;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub use crate::llm::tools::function_schema_from_catalog_row;
pub use project::{TOOL_CALL_RECEIPT_VERSION, TOOL_RESULT_RECEIPT_VERSION};
pub use validate::{validate_training_example_pairing, TrainingPairingError};

/// Schema version of the projected example. Bump only for a breaking change to
/// the shape below; consumers pin on this exact string.
pub const TRAINING_EXAMPLE_SCHEMA_VERSION: &str = "harn.agent_training_example.v1";

/// Structured reason a run could not be projected. `kind` is a small stable
/// vocabulary so callers can branch without matching on prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingExampleError {
    pub kind: String,
    pub message: String,
    /// 1-based JSONL row that produced the failure, when it came from a row.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub event_index: Option<usize>,
}

impl TrainingExampleError {
    pub(crate) fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
            event_index: None,
        }
    }

    pub(crate) fn at(kind: &str, event_index: usize, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
            event_index: Some(event_index),
        }
    }
}

impl std::fmt::Display for TrainingExampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.event_index {
            Some(index) => write!(f, "{} (event {index}): {}", self.kind, self.message),
            None => write!(f, "{}: {}", self.kind, self.message),
        }
    }
}

impl std::error::Error for TrainingExampleError {}

/// One provider-visible turn, in Harn's canonical provider-independent shape.
///
/// Tool results are always `role: "tool"` carrying `tool_call_id`, even when
/// the run served them to the model as a text-channel `user` echo. `content`
/// stays byte-exact with what the model saw, so a target renderer can
/// reproduce either channel without re-parsing anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<TrainingToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: TrainingToolFunction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingToolFunction {
    pub name: String,
    pub arguments: JsonValue,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingUsage {
    pub provider_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Identity and digest of the exact bytes this example was projected from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingSource {
    pub descriptor_schema_version: String,
    pub transcript_path: String,
    pub transcript_sha256: String,
    pub transcript_byte_len: u64,
    pub event_count: usize,
    pub first_event_index: usize,
    pub last_event_index: usize,
    pub first_event_id: String,
    pub last_event_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingProvenance {
    pub run_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_id: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_policy: Option<String>,
    /// The channel the session claimed and served tool results on, when the
    /// run recorded one. An escalation can dispatch a call on a different
    /// effective format without re-claiming the session's, so the two are
    /// recorded separately rather than collapsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_tool_format: Option<String>,
    /// Format the calls actually dispatched with.
    pub effective_tool_format: String,
    /// Hash of the exact schema array the model was served.
    pub tool_catalog_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_catalog_content_hash: Option<String>,
    pub terminal_status: String,
    pub usage: TrainingUsage,
    pub source: TrainingSource,
}

/// The `harn.agent_training_example.v1` contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTrainingExample {
    pub schema_version: String,
    pub messages: Vec<TrainingMessage>,
    /// The effective tool catalog exactly as served, never inferred from
    /// prompt prose or observed argument values.
    pub tools: Vec<JsonValue>,
    pub provenance: TrainingProvenance,
}

/// Which run inside the artifact to project. Selection is always by explicit
/// identity; nothing here may be chosen by output length, verbosity, or a
/// `DONE` substring.
#[derive(Clone, Debug, Default)]
pub struct TrainingExampleRequest {
    pub run_record_path: PathBuf,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
}

impl TrainingExampleRequest {
    pub fn new(run_record_path: impl Into<PathBuf>) -> Self {
        Self {
            run_record_path: run_record_path.into(),
            run_id: None,
            session_id: None,
        }
    }
}

/// Project one authoritative run into one training example.
///
/// Fails with a structured error rather than degrading whenever the run record
/// cannot supply a required source fact, the transcript does not verify
/// against its descriptor, or the recorded turns violate the call/result
/// pairing invariant.
pub fn project_agent_training_example(
    request: &TrainingExampleRequest,
) -> Result<AgentTrainingExample, TrainingExampleError> {
    let run_record_path = request.run_record_path.as_path();
    let run = load_run(run_record_path)?;
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
    let events = source::load_events(&transcript_path)?;
    project::project(&run, &descriptor, &transcript_path, &events, request)
}

/// Read the run record exactly as it was persisted.
///
/// Deliberately not [`load_run_record`](crate::orchestration::load_run_record):
/// that helper refreshes observability from the sidecar currently on disk, so
/// the descriptor it hands back is a fresh re-derivation and its digest can
/// never disagree with the bytes it was just computed from. Authority needs
/// the descriptor as *written*, so a sidecar edited after the run finalized is
/// caught by the digest comparison instead of being silently re-blessed.
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
