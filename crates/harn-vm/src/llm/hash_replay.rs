//! Hash-addressed LLM record/replay (`HARN_LLM_RECORD` / `HARN_LLM_REPLAY`).
//!
//! Each request is hashed and its response stored as `<dir>/<hash>.json`.
//! This is separate from the ordered JSONL mock fixtures in [`super::mock`]:
//! a hash fixture is keyed by request content, not by call order or scope.

use std::cell::RefCell;

use super::api::{LlmResult, RawProviderToolCall};
use super::mock::DEFAULT_MOCK_SCOPE;

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
}

/// LLM replay mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmReplayMode {
    Off,
    Record,
    Replay,
}

/// Set LLM replay mode (record/replay) and fixture directory.
pub fn set_replay_mode(mode: LlmReplayMode, fixture_dir: &str) {
    LLM_REPLAY_MODE.with(|v| *v.borrow_mut() = mode);
    LLM_FIXTURE_DIR.with(|v| *v.borrow_mut() = fixture_dir.to_string());
}

pub(crate) fn get_replay_mode() -> LlmReplayMode {
    LLM_REPLAY_MODE.with(|v| *v.borrow())
}

pub(crate) fn get_fixture_dir() -> String {
    LLM_FIXTURE_DIR.with(|v| v.borrow().clone())
}

/// Hash a request for fixture file naming using canonical JSON serialization.
pub(crate) fn fixture_hash(
    model: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    mock_scope: Option<&str>,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut hasher);
    // Canonical JSON hashing is stable across Debug-format changes.
    crate::canonical_json::to_string(&serde_json::Value::Array(messages.to_vec()))
        .hash(&mut hasher);
    system.hash(&mut hasher);
    if mock_scope.is_some_and(|scope| scope != DEFAULT_MOCK_SCOPE) {
        mock_scope.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

pub(crate) fn fixture_hash_for_request(request: &super::api::LlmRequestPayload) -> String {
    fixture_hash(
        &request.model,
        &request.messages,
        request.system.as_deref(),
        request.mock_scope.as_deref(),
    )
}

pub(crate) fn save_fixture(hash: &str, result: &LlmResult) {
    let dir = get_fixture_dir();
    if dir.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/{hash}.json");
    let json = serde_json::json!({
        "text": result.text,
        "tool_calls": result.tool_calls,
        "raw_tool_calls": result.raw_tool_calls,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        "model": result.model,
        "provider": result.provider,
        "thinking": result.thinking,
        "thinking_summary": result.thinking_summary,
        "stop_reason": result.stop_reason,
        "blocks": result.blocks,
        "logprobs": result.logprobs,
        "telemetry": result.telemetry,
    });
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    );
}

pub(crate) fn load_fixture(hash: &str) -> Option<LlmResult> {
    let dir = get_fixture_dir();
    if dir.is_empty() {
        return None;
    }
    let path = format!("{dir}/{hash}.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    Some(LlmResult {
        attempts: Default::default(),
        text_projection: None,
        served_fast: false,
        text: json["text"].as_str().unwrap_or("").to_string(),
        tool_calls: json["tool_calls"].as_array().cloned().unwrap_or_default(),
        raw_tool_calls: RawProviderToolCall::array_from_value(&json["raw_tool_calls"]).ok()?,
        input_tokens: json["input_tokens"].as_i64().unwrap_or(0),
        output_tokens: json["output_tokens"].as_i64().unwrap_or(0),
        cache_read_tokens: json["cache_read_tokens"].as_i64().unwrap_or(0),
        cache_write_tokens: json["cache_write_tokens"]
            .as_i64()
            .or_else(|| json["cache_creation_input_tokens"].as_i64())
            .unwrap_or(0),
        cache_supported: json["cache_supported"].as_bool().unwrap_or(true),
        model: json["model"].as_str().unwrap_or("").to_string(),
        provider: json["provider"].as_str().unwrap_or("mock").to_string(),
        thinking: json["thinking"].as_str().map(|s| s.to_string()),
        thinking_summary: json["thinking_summary"].as_str().map(|s| s.to_string()),
        stop_reason: json["stop_reason"].as_str().map(|s| s.to_string()),
        blocks: json["blocks"].as_array().cloned().unwrap_or_default(),
        logprobs: json["logprobs"].as_array().cloned().unwrap_or_default(),
        telemetry: serde_json::from_value(json["telemetry"].clone()).unwrap_or_default(),
    })
}
