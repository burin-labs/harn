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

/// Temporarily select a fixture mode on the calling thread.
pub fn scope_replay_mode(mode: LlmReplayMode, fixture_dir: &str) -> ReplayModeScope {
    let previous = ReplayModeScope {
        mode: get_replay_mode(),
        fixture_dir: get_fixture_dir(),
        thread_bound: std::marker::PhantomData,
    };
    set_replay_mode(mode, fixture_dir);
    previous
}

/// Restores the caller's fixture mode when an invocation finishes or unwinds.
#[must_use = "dropping the scope immediately restores the previous fixture mode"]
pub struct ReplayModeScope {
    mode: LlmReplayMode,
    fixture_dir: String,
    thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for ReplayModeScope {
    fn drop(&mut self) {
        set_replay_mode(self.mode, &self.fixture_dir);
    }
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

#[cfg(test)]
pub(crate) fn save_fixture(hash: &str, result: &LlmResult) {
    let dir = get_fixture_dir();
    save_fixture_at(&dir, hash, result);
}

/// A caller's record destination stays valid when transport outlives it.
#[derive(Clone)]
pub(crate) struct FixtureRecorder {
    directory: String,
    hash: String,
}

impl FixtureRecorder {
    pub(crate) fn capture(hash: &str) -> Option<Self> {
        (get_replay_mode() == LlmReplayMode::Record).then(|| Self {
            directory: get_fixture_dir(),
            hash: hash.to_owned(),
        })
    }

    pub(crate) fn record(&self, result: &LlmResult) {
        save_fixture_at(&self.directory, &self.hash, result);
    }
}

fn save_fixture_at(dir: &str, hash: &str, result: &LlmResult) {
    if dir.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let path = format!("{dir}/{hash}.json");
    let Ok(serde_json::Value::Object(mut json)) = serde_json::to_value(result) else {
        return;
    };
    // Retry accounting belongs to the current logical call. Provider receipts
    // survive replay; derived text projection already skips serde.
    json.remove("attempts");
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
        served_fast: json["served_fast"].as_bool().unwrap_or(false),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{test_text_projection, ProviderAttempts};

    #[test]
    fn fixture_mode_scope_restores_mode_and_directory_after_unwind() {
        let _outer = scope_replay_mode(LlmReplayMode::Record, "outer-fixtures");
        let result = std::panic::catch_unwind(|| {
            let _inner = scope_replay_mode(LlmReplayMode::Replay, "inner-fixtures");
            assert_eq!(get_replay_mode(), LlmReplayMode::Replay);
            assert_eq!(get_fixture_dir(), "inner-fixtures");
            panic!("invocation failed");
        });
        assert!(result.is_err());
        assert_eq!(get_replay_mode(), LlmReplayMode::Record);
        assert_eq!(get_fixture_dir(), "outer-fixtures");
    }

    #[test]
    fn recording_round_trips_provider_receipts_without_attempt_context() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_str().unwrap();
        for (cache_supported, served_fast) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut result = LlmResult {
                text: "recorded provider result".to_string(),
                input_tokens: 29,
                output_tokens: 3,
                cache_supported,
                model: "recording-proof".to_string(),
                provider: "ollama".to_string(),
                stop_reason: Some("stop".to_string()),
                attempts: ProviderAttempts {
                    total: 3,
                    rate_limited: 2,
                    ..Default::default()
                },
                served_fast,
                ..Default::default()
            };
            result.text_projection = Some(Box::new(test_text_projection(&result, None)));
            let hash = format!("cache-{cache_supported}-fast-{served_fast}");
            set_replay_mode(LlmReplayMode::Record, path);
            let recorder =
                FixtureRecorder::capture(&hash).expect("record mode captures destination");
            recorder.record(&result);
            set_replay_mode(LlmReplayMode::Replay, path);
            let replayed = load_fixture(&hash).expect("recorded fixture loads");
            assert_eq!(replayed.cache_supported, cache_supported);
            assert_eq!(replayed.served_fast, served_fast);
            result.attempts = Default::default();
            result.text_projection = None;
            assert_eq!(replayed, result, "provider result changed during replay");
        }
        std::fs::write(directory.path().join("legacy.json"), r#"{"text":"legacy"}"#).unwrap();
        let legacy = load_fixture("legacy").expect("legacy fixture loads");
        assert_eq!(legacy.text, "legacy");
        assert!(legacy.cache_supported);
        assert!(!legacy.served_fast);
        set_replay_mode(LlmReplayMode::Off, "");
    }
}
