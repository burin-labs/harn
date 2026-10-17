//! Deterministic projection of verified transcript events into training examples.
//!
//! Native adapters verify persisted run identity, transcript digest and terminal
//! state before supplying the source metadata and exact transcript bytes here.
//! This module owns event versions, turn ordering, call/result pairing, and
//! provenance projection. Missing or conflicting facts fail with a structured
//! error instead of producing a partial example.

mod project;
mod source;
mod validate;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

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
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
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
    /// Harn-owned execution identity from the source run's evidence envelope.
    /// Legacy run records leave this absent rather than synthesizing identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
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

/// Source facts from the persisted run and its verified transcript descriptor.
/// Adapters must verify the digest and terminal state before calling the projector.
#[derive(Clone, Debug)]
pub struct TrainingProjectionSource {
    pub run_id: String,
    pub execution_id: Option<String>,
    pub stage_ids: Vec<String>,
    pub recorded_session_id: Option<String>,
    pub descriptor_schema_version: String,
    pub transcript_path: PathBuf,
    pub transcript_sha256: String,
    pub transcript_byte_len: u64,
    pub terminal_status: String,
}

/// Project exact, verified transcript bytes without filesystem access.
pub fn project_training_transcript(
    source: &TrainingProjectionSource,
    text: &str,
    requested_session_id: Option<&str>,
) -> Result<AgentTrainingExample, TrainingExampleError> {
    let events = source::parse_events(text, &source.transcript_path.display().to_string())?;
    project::project(source, &events, requested_session_id)
}
