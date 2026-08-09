//! Strict load of the LLM transcript sidecar.
//!
//! Strictness is not optional here. A lenient reader that drops a row it
//! cannot parse produces an example that silently omits a turn, and nothing
//! downstream can tell that apart from a run that genuinely had fewer turns.
//! Every row either parses into a typed event or the whole projection fails.

use std::path::Path;

use serde_json::Value as JsonValue;

use super::TrainingExampleError;

/// Event kinds the projector reads. Anything else in the stream is carried as
/// [`SourceEventKind::Other`] and ignored — the sidecar is a general
/// observability log and gains new diagnostic events over time, so an
/// unrecognised *kind* is not an error. An unrecognised *version* of a kind we
/// do read is, because that means the payload we are about to interpret may
/// not mean what we think.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceEventKind {
    SystemPrompt,
    ToolSchemas,
    ProviderCallRequest,
    ProviderCallResponse,
    Message,
    ToolCall,
    ToolResult,
    Other,
}

/// Schema versions this projector understands, per event kind it reads.
///
/// An event may omit `schema_version` entirely: the long-lived sidecar events
/// predate versioning and their shapes are pinned by the projector's own
/// tests. Once an event *declares* a version, it must be one we know.
fn known_version(kind: SourceEventKind, version: &str) -> bool {
    match kind {
        SourceEventKind::ToolResult => version == super::project::TOOL_RESULT_RECEIPT_VERSION,
        SourceEventKind::ToolCall => version == super::project::TOOL_CALL_RECEIPT_VERSION,
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SourceEvent {
    /// 1-based row number in the JSONL file.
    pub index: usize,
    pub kind: SourceEventKind,
    pub value: JsonValue,
}

impl SourceEvent {
    pub(crate) fn str_field(&self, key: &str) -> Option<&str> {
        self.value.get(key).and_then(JsonValue::as_str)
    }

    pub(crate) fn u64_field(&self, key: &str) -> Option<u64> {
        self.value.get(key).and_then(JsonValue::as_u64)
    }

    pub(crate) fn identity(&self) -> String {
        self.value
            .get("id")
            .or_else(|| self.value.get("event_id"))
            .or_else(|| self.value.get("call_id"))
            .and_then(JsonValue::as_str)
            .map_or_else(|| format!("row:{}", self.index), str::to_string)
    }
}

fn classify(type_name: &str) -> SourceEventKind {
    match type_name {
        "system_prompt" => SourceEventKind::SystemPrompt,
        "tool_schemas" => SourceEventKind::ToolSchemas,
        "provider_call_request" => SourceEventKind::ProviderCallRequest,
        "provider_call_response" => SourceEventKind::ProviderCallResponse,
        "message" => SourceEventKind::Message,
        "tool_call" => SourceEventKind::ToolCall,
        "tool_result" => SourceEventKind::ToolResult,
        _ => SourceEventKind::Other,
    }
}

pub(crate) fn load_events(path: &Path) -> Result<Vec<SourceEvent>, TrainingExampleError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        TrainingExampleError::new(
            "unavailable",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    parse_events(&text, &path.display().to_string())
}

pub(crate) fn parse_events(
    text: &str,
    label: &str,
) -> Result<Vec<SourceEvent>, TrainingExampleError> {
    let mut events = Vec::new();
    let mut index = 0;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        index += 1;
        let value: JsonValue = serde_json::from_str(line).map_err(|error| {
            TrainingExampleError::at(
                "malformed_jsonl",
                index,
                format!("{label}: row is not valid JSON: {error}"),
            )
        })?;
        let Some(type_name) = value.get("type").and_then(JsonValue::as_str) else {
            return Err(TrainingExampleError::at(
                "malformed_jsonl",
                index,
                format!("{label}: row has no `type` discriminator"),
            ));
        };
        let type_name = type_name.to_string();
        let kind = classify(&type_name);
        if kind != SourceEventKind::Other {
            if let Some(version) = value.get("schema_version").and_then(JsonValue::as_str) {
                if !known_version(kind, version) {
                    return Err(TrainingExampleError::at(
                        "unknown_event_version",
                        index,
                        format!("{label}: `{type_name}` declares unsupported schema {version}"),
                    ));
                }
            }
        }
        events.push(SourceEvent { index, kind, value });
    }
    if events.is_empty() {
        return Err(TrainingExampleError::new(
            "empty_transcript",
            format!("{label} contains no JSONL events"),
        ));
    }
    Ok(events)
}
