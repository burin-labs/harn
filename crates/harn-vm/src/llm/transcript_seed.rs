//! Import prompt-visible messages from LLM transcript JSONL sidecars.
//!
//! The importer accepts both Harn's current sidecar event stream and older
//! request/response fixtures that carried full request snapshots. Exact prefix
//! replay requires either `message` events or request snapshots; response-only
//! sidecars can be imported only with validation disabled because they are
//! missing user/tool-result turns.

use std::path::Path;

use serde_json::{Map, Value};

use crate::llm::tools::build_assistant_response_message;

#[derive(Clone, Debug)]
pub(crate) struct SeedOptions {
    pub truncate_to_last: Option<usize>,
    pub drop_tool_calls: bool,
    pub validate: bool,
    pub target_provider: Option<String>,
    pub target_model: Option<String>,
}

impl Default for SeedOptions {
    fn default() -> Self {
        Self {
            truncate_to_last: None,
            drop_tool_calls: false,
            validate: true,
            target_provider: None,
            target_model: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SeedSourceFormat {
    MessageEvents,
    RequestSnapshots,
    ProviderResponsesOnly,
}

impl SeedSourceFormat {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::MessageEvents => "message_events",
            Self::RequestSnapshots => "request_snapshots",
            Self::ProviderResponsesOnly => "provider_responses_only",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SeededTranscript {
    pub messages: Vec<Value>,
    pub system_prompt: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tool_format: Option<String>,
    pub record_count: usize,
    pub source_format: SeedSourceFormat,
    pub partial: bool,
    pub truncated: bool,
}

pub(crate) fn load_seeded_transcript_from_jsonl(
    path: &Path,
    options: &SeedOptions,
) -> Result<SeededTranscript, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let records = parse_jsonl_records(&content, path)?;
    let has_message_events = records.iter().any(is_message_record);
    let has_request_snapshots = records
        .iter()
        .any(|record| request_messages(record).is_some());
    let source_format = if has_message_events {
        SeedSourceFormat::MessageEvents
    } else if has_request_snapshots {
        SeedSourceFormat::RequestSnapshots
    } else {
        SeedSourceFormat::ProviderResponsesOnly
    };

    let mut state = ImportState::default();
    for record in &records {
        state.ingest_metadata(record);
        match source_format {
            SeedSourceFormat::MessageEvents => {
                state.messages.extend(messages_from_record(record));
            }
            SeedSourceFormat::RequestSnapshots => {
                if let Some(messages) = request_messages(record) {
                    state.messages = messages;
                    continue;
                }
                if is_response_record(record) {
                    if let Some(message) = assistant_message_from_response(record, &state) {
                        state.messages.push(message);
                    }
                }
            }
            SeedSourceFormat::ProviderResponsesOnly => {
                if is_response_record(record) {
                    if let Some(message) = assistant_message_from_response(record, &state) {
                        state.messages.push(message);
                    }
                }
            }
        }
    }

    let original_message_count = state.messages.len();
    let validation_state = state.clone();
    let mut messages = state.messages;
    if options.drop_tool_calls {
        messages = drop_tool_payloads(&messages);
    }
    let mut truncated = false;
    if let Some(keep_last) = options.truncate_to_last {
        let start = messages.len().saturating_sub(keep_last);
        truncated = start > 0;
        messages = messages.into_iter().skip(start).collect();
    }

    if options.validate {
        validate_reconstruction(
            &messages,
            original_message_count,
            &source_format,
            &validation_state,
            options,
        )?;
    }

    Ok(SeededTranscript {
        messages,
        system_prompt: state.system_prompt,
        provider: state.provider,
        model: state.model,
        tool_format: state.tool_format,
        record_count: records.len(),
        partial: source_format == SeedSourceFormat::ProviderResponsesOnly,
        source_format,
        truncated,
    })
}

#[derive(Clone, Debug, Default)]
struct ImportState {
    messages: Vec<Value>,
    system_prompt: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    tool_format: Option<String>,
}

impl ImportState {
    fn ingest_metadata(&mut self, record: &Value) {
        match event_type(record) {
            Some("system_prompt") => {
                if let Some(content) = string_field(record, "content") {
                    self.system_prompt = Some(content);
                }
            }
            Some("provider_call_request" | "request") => {
                if let Some(system) = request_system(record) {
                    self.system_prompt = Some(system);
                }
                if let Some(provider) = string_field(record, "provider") {
                    self.provider = Some(provider);
                }
                if let Some(model) = string_field(record, "model") {
                    self.model = Some(model);
                }
                if let Some(tool_format) = string_field(record, "tool_format") {
                    self.tool_format = Some(tool_format);
                }
            }
            Some("provider_call_response" | "response") => {
                if self.provider.is_none() {
                    self.provider = string_field(record, "provider");
                }
                if self.model.is_none() {
                    self.model = string_field(record, "model");
                }
            }
            _ => {}
        }
    }
}

fn parse_jsonl_records(content: &str, path: &Path) -> Result<Vec<Value>, String> {
    let mut records = Vec::new();
    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "invalid JSON in {} line {}: {error}",
                path.display(),
                index + 1
            )
        })?;
        if !value.is_object() {
            return Err(format!(
                "invalid transcript record in {} line {}: expected JSON object",
                path.display(),
                index + 1
            ));
        }
        records.push(value);
    }
    Ok(records)
}

fn validate_reconstruction(
    messages: &[Value],
    original_message_count: usize,
    source_format: &SeedSourceFormat,
    state: &ImportState,
    options: &SeedOptions,
) -> Result<(), String> {
    let explicit_empty = options.truncate_to_last == Some(0);
    if original_message_count == 0 && !explicit_empty {
        return Err("transcript JSONL did not contain any prompt-visible messages".to_string());
    }
    if messages.is_empty() && !explicit_empty {
        return Err("seed options dropped every reconstructed message".to_string());
    }
    if source_format == &SeedSourceFormat::ProviderResponsesOnly {
        return Err(
            "transcript JSONL has provider responses but no message events or request snapshots; \
             exact prefix reconstruction is impossible (pass validate: false for best-effort assistant-only import)"
                .to_string(),
        );
    }
    if let Some(expected) = options.target_provider.as_deref() {
        if state.provider.as_deref() != Some(expected) {
            let actual = state.provider.as_deref().unwrap_or("unknown");
            return Err(format!(
                "transcript provider '{actual}' does not match requested provider '{expected}'"
            ));
        }
    }
    if let Some(expected) = options.target_model.as_deref() {
        if state.model.as_deref() != Some(expected) {
            let actual = state.model.as_deref().unwrap_or("unknown");
            return Err(format!(
                "transcript model '{actual}' does not match requested model '{expected}'"
            ));
        }
    }
    Ok(())
}

fn event_type(record: &Value) -> Option<&str> {
    record
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| record.get("kind").and_then(Value::as_str))
}

fn string_field(record: &Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn is_message_record(record: &Value) -> bool {
    matches!(
        event_type(record),
        Some("message" | "tool_result" | "tool_results")
    )
}

fn is_response_record(record: &Value) -> bool {
    matches!(
        event_type(record),
        Some("provider_call_response" | "response")
    )
}

fn request_system(record: &Value) -> Option<String> {
    string_field(record, "system").or_else(|| {
        record
            .get("request_snapshot")
            .and_then(|snapshot| snapshot.get("system"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
    })
}

fn request_messages(record: &Value) -> Option<Vec<Value>> {
    let raw_messages = record
        .get("request_snapshot")
        .and_then(|snapshot| snapshot.get("messages"))
        .or_else(|| record.get("messages"))?;
    let items = raw_messages.as_array()?;
    let messages = items
        .iter()
        .filter_map(normalize_message)
        .collect::<Vec<_>>();
    Some(messages)
}

fn message_from_record(record: &Value) -> Option<Value> {
    if let Some(message) = record.get("message").and_then(normalize_message) {
        return Some(message);
    }
    if event_type(record) == Some("tool_results") {
        return None;
    }
    let role = string_field(record, "role").or_else(|| match event_type(record) {
        Some("tool_result") => Some("tool_result".to_string()),
        _ => None,
    })?;
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String(role));
    if let Some(content) = record.get("content") {
        message.insert("content".to_string(), content.clone());
    } else if let Some(text) = string_field(record, "text") {
        message.insert("content".to_string(), Value::String(text));
    } else {
        message.insert("content".to_string(), Value::String(String::new()));
    }
    for key in [
        "name",
        "tool_call_id",
        "tool_use_id",
        "tool_calls",
        "native_tool_calls",
        "reasoning",
    ] {
        if let Some(value) = record.get(key) {
            message.insert(key.to_string(), value.clone());
        }
    }
    normalize_message(&Value::Object(message))
}

fn messages_from_record(record: &Value) -> Vec<Value> {
    if event_type(record) == Some("tool_results") {
        if let Some(messages) = record.get("messages").and_then(Value::as_array) {
            return messages.iter().filter_map(normalize_message).collect();
        }
        return record
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(tool_result_message_from_result)
            .collect();
    }
    message_from_record(record).into_iter().collect()
}

fn tool_result_message_from_result(result: &Value) -> Option<Value> {
    let object = result.as_object()?;
    let name = object
        .get("tool_name")
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = object
        .get("observation")
        .or_else(|| object.get("rendered_result"))
        .or_else(|| object.get("output"))
        .or_else(|| object.get("content"))
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()));
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("tool".to_string()));
    if !name.is_empty() {
        message.insert("name".to_string(), Value::String(name.to_string()));
    }
    if let Some(tool_call_id) = object
        .get("tool_call_id")
        .or_else(|| object.get("tool_use_id"))
        .and_then(Value::as_str)
    {
        message.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.to_string()),
        );
    }
    message.insert("content".to_string(), content);
    normalize_message(&Value::Object(message))
}

fn normalize_message(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.trim().is_empty())?;
    let mut normalized = object.clone();
    normalized.insert("role".to_string(), Value::String(role.to_string()));
    if !normalized.contains_key("content")
        && !normalized.contains_key("tool_calls")
        && !normalized.contains_key("native_tool_calls")
    {
        return None;
    }
    Some(Value::Object(normalized))
}

fn assistant_message_from_response(record: &Value, state: &ImportState) -> Option<Value> {
    let text = string_field(record, "text").unwrap_or_default();
    let blocks = record
        .get("blocks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tool_calls = record
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if text.is_empty() && blocks.is_empty() && tool_calls.is_empty() {
        return None;
    }
    let provider = string_field(record, "provider")
        .or_else(|| state.provider.clone())
        .unwrap_or_default();
    let reasoning = string_field(record, "thinking");
    let message = build_assistant_response_message(
        &text,
        &blocks,
        &tool_calls,
        reasoning.as_deref(),
        &provider,
    );
    normalize_message(&message)
}

fn drop_tool_payloads(messages: &[Value]) -> Vec<Value> {
    messages.iter().filter_map(drop_tool_payload).collect()
}

fn drop_tool_payload(message: &Value) -> Option<Value> {
    let mut object = message.as_object()?.clone();
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if matches!(role.as_str(), "tool" | "tool_result") {
        return None;
    }
    object.remove("tool_calls");
    object.remove("native_tool_calls");
    object.remove("tool_call_id");
    object.remove("tool_use_id");
    if let Some(content) = object.get("content").cloned() {
        object.insert("content".to_string(), text_only_content(content)?);
    }
    if role == "assistant" && message_text_is_empty(&Value::Object(object.clone())) {
        return None;
    }
    Some(Value::Object(object))
}

fn text_only_content(content: Value) -> Option<Value> {
    match content {
        Value::String(_) => Some(content),
        Value::Array(blocks) => {
            let kept = blocks
                .into_iter()
                .filter(|block| {
                    let block_type = block.get("type").and_then(Value::as_str);
                    matches!(block_type, Some("text") | None)
                })
                .collect::<Vec<_>>();
            if kept.is_empty() {
                None
            } else {
                Some(Value::Array(kept))
            }
        }
        other => Some(other),
    }
}

fn message_text_is_empty(message: &Value) -> bool {
    match message.get("content") {
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Array(blocks)) => blocks.is_empty(),
        Some(Value::Null) | None => true,
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn load(content: &str, options: SeedOptions) -> Result<SeededTranscript, String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("llm_transcript.jsonl");
        std::fs::write(&path, content).expect("write fixture");
        load_seeded_transcript_from_jsonl(&path, &options)
    }

    fn load_records(records: Vec<Value>, options: SeedOptions) -> Result<SeededTranscript, String> {
        let mut content = records
            .into_iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        content.push('\n');
        load(&content, options)
    }

    #[test]
    fn imports_message_events_exactly() {
        let seeded = load_records(
            vec![
                json!({"type": "system_prompt", "content": "sys"}),
                json!({
                    "type": "provider_call_request",
                    "provider": "mock",
                    "model": "m",
                    "tool_format": "native",
                    "message_count": 1,
                }),
                json!({"type": "message", "message": {"role": "user", "content": "hello"}}),
                json!({"type": "message", "message": {"role": "assistant", "content": "hi"}}),
            ],
            SeedOptions::default(),
        )
        .expect("seed transcript");

        assert_eq!(seeded.source_format, SeedSourceFormat::MessageEvents);
        assert_eq!(seeded.system_prompt.as_deref(), Some("sys"));
        assert_eq!(seeded.provider.as_deref(), Some("mock"));
        assert_eq!(seeded.model.as_deref(), Some("m"));
        assert_eq!(seeded.tool_format.as_deref(), Some("native"));
        assert_eq!(seeded.messages.len(), 2);
        assert_eq!(seeded.messages[0]["content"], "hello");
        assert_eq!(seeded.messages[1]["content"], "hi");
    }

    #[test]
    fn imports_legacy_request_snapshots_and_final_response() {
        let seeded = load_records(
            vec![
                json!({
                    "type": "request",
                    "system": "sys",
                    "provider": "openrouter",
                    "model": "m",
                    "tool_format": "text",
                    "messages": [{"role": "user", "content": "hello"}],
                }),
                json!({
                    "type": "response",
                    "provider": "openrouter",
                    "model": "m",
                    "text": "hi",
                    "tool_calls": [],
                }),
                json!({
                    "type": "request",
                    "provider": "openrouter",
                    "model": "m",
                    "messages": [
                        {"role": "user", "content": "hello"},
                        {"role": "assistant", "content": "hi"},
                        {
                            "role": "tool",
                            "name": "lookup",
                            "tool_call_id": "call_1",
                            "content": "ok",
                        },
                    ],
                }),
                json!({
                    "type": "response",
                    "provider": "openrouter",
                    "model": "m",
                    "text": "done",
                    "tool_calls": [],
                }),
            ],
            SeedOptions::default(),
        )
        .expect("seed transcript");

        assert_eq!(seeded.source_format, SeedSourceFormat::RequestSnapshots);
        assert_eq!(seeded.messages.len(), 4);
        assert_eq!(seeded.messages[2]["role"], "tool");
        assert_eq!(seeded.messages[3]["content"], "done");
    }

    #[test]
    fn imports_batched_tool_result_events() {
        let seeded = load_records(
            vec![
                json!({"type": "message", "message": {"role": "user", "content": "hello"}}),
                json!({
                    "type": "tool_results",
                    "results": [{
                        "tool_name": "lookup",
                        "tool_call_id": "call_1",
                        "observation": "ok",
                    }],
                }),
            ],
            SeedOptions::default(),
        )
        .expect("seed transcript");

        assert_eq!(seeded.messages.len(), 2);
        assert_eq!(seeded.messages[1]["role"], "tool");
        assert_eq!(seeded.messages[1]["name"], "lookup");
        assert_eq!(seeded.messages[1]["content"], "ok");
    }

    #[test]
    fn drops_tool_payloads_and_truncates_after_filtering() {
        let seeded = load_records(
            vec![
                json!({"type": "message", "message": {"role": "user", "content": "hello"}}),
                json!({
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": "call_1",
                            "name": "lookup",
                            "arguments": {"q": "x"},
                        }],
                    },
                }),
                json!({
                    "type": "message",
                    "message": {
                        "role": "tool",
                        "tool_call_id": "call_1",
                        "name": "lookup",
                        "content": "result",
                    },
                }),
                json!({"type": "message", "message": {"role": "assistant", "content": "done"}}),
            ],
            SeedOptions {
                truncate_to_last: Some(2),
                drop_tool_calls: true,
                ..SeedOptions::default()
            },
        )
        .expect("seed transcript");

        assert_eq!(seeded.messages.len(), 2);
        assert_eq!(seeded.messages[0]["role"], "user");
        assert_eq!(seeded.messages[1]["content"], "done");
        assert!(!seeded.messages[1]
            .as_object()
            .unwrap()
            .contains_key("tool_calls"));
    }

    #[test]
    fn rejects_response_only_transcripts_by_default() {
        let error = load_records(
            vec![json!({
                "type": "provider_call_response",
                "provider": "mock",
                "model": "m",
                "text": "hi",
                "tool_calls": [],
            })],
            SeedOptions::default(),
        )
        .expect_err("response-only transcript is partial");

        assert!(error.contains("exact prefix reconstruction is impossible"));
    }

    #[test]
    fn can_import_response_only_transcripts_when_validation_is_disabled() {
        let seeded = load_records(
            vec![json!({
                "type": "provider_call_response",
                "provider": "mock",
                "model": "m",
                "text": "hi",
                "tool_calls": [],
            })],
            SeedOptions {
                validate: false,
                ..SeedOptions::default()
            },
        )
        .expect("best-effort import");

        assert!(seeded.partial);
        assert_eq!(seeded.messages.len(), 1);
        assert_eq!(seeded.messages[0]["content"], "hi");
    }
}
