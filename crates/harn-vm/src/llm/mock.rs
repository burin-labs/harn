use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use super::api::{LlmResult, ProviderTelemetry};
use crate::orchestration::ToolCallRecord;
use crate::value::{ErrorCategory, VmError, VmValue};

/// LLM replay mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmReplayMode {
    Off,
    Record,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliLlmMockMode {
    Off,
    Replay,
    Record,
}

/// Categorized error injected by a mock. When present, the mock
/// short-circuits the provider call and surfaces as
/// `VmError::CategorizedError`, so `llm_call` throws and
/// `llm_call_safe` populates its `error` envelope.
#[derive(Clone)]
pub struct MockError {
    pub category: ErrorCategory,
    pub message: String,
    pub status: Option<u16>,
    pub kind: Option<String>,
    pub reason: Option<String>,
    /// Optional retry hint. Provider-envelope mocks put this directly
    /// on the thrown dict; legacy category-only mocks embed it in the
    /// message so the live-provider parser path still exercises the
    /// same extraction code.
    pub retry_after_ms: Option<u64>,
}

impl MockError {
    fn has_provider_envelope(&self) -> bool {
        self.status.is_some() || self.kind.is_some() || self.reason.is_some()
    }
}

pub(crate) fn build_mock_error(
    category: Option<String>,
    message: Option<String>,
    status: Option<u16>,
    kind: Option<String>,
    reason: Option<String>,
    retry_after_ms: Option<u64>,
) -> Result<MockError, String> {
    if retry_after_ms.is_some_and(|ms| ms > i64::MAX as u64) {
        return Err("error.retry_after_ms must fit in a signed 64-bit integer".to_string());
    }
    let kind = match kind {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            if super::api::LlmErrorKind::parse(&normalized).is_none() {
                return Err(format!("unknown error kind `{value}`"));
            }
            Some(normalized)
        }
        None => None,
    };
    let reason = reason.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let category_was_provided = category.is_some();
    let category = match category {
        Some(value) if value.trim().is_empty() => {
            return Err("error.category must not be empty".to_string());
        }
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            let category = ErrorCategory::parse(&normalized);
            if category.as_str() != normalized {
                return Err(format!("unknown error category `{value}`"));
            }
            category
        }
        None => infer_mock_error_category(status, kind.as_deref(), reason.as_deref()),
    };
    if !category_was_provided && kind.is_none() && status.is_none() && reason.is_none() {
        return Err(
            "error.category is required unless error.status, error.kind, or error.reason is set"
                .to_string(),
        );
    }
    Ok(MockError {
        category,
        message: message.unwrap_or_else(|| {
            default_mock_error_message(status, kind.as_deref(), reason.as_deref())
        }),
        status,
        kind,
        reason,
        retry_after_ms,
    })
}

pub(crate) fn validate_mock_error_status(status: i64) -> Result<u16, String> {
    let status = u16::try_from(status)
        .map_err(|_| "error.status must be an HTTP status code".to_string())?;
    reqwest::StatusCode::from_u16(status)
        .map_err(|_| "error.status must be an HTTP status code".to_string())?;
    Ok(status)
}

fn infer_mock_error_category(
    status: Option<u16>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> ErrorCategory {
    if let Some(status) = status {
        match status {
            401 | 403 => return ErrorCategory::Auth,
            404 | 410 => return ErrorCategory::NotFound,
            408 | 504 | 522 | 524 => return ErrorCategory::Timeout,
            429 => return ErrorCategory::RateLimit,
            503 | 529 => return ErrorCategory::Overloaded,
            500 | 502 => return ErrorCategory::ServerError,
            _ => {}
        }
    }
    if let Some(reason) = reason {
        match reason {
            "rate_limit" => return ErrorCategory::RateLimit,
            "timeout" => return ErrorCategory::Timeout,
            "network_error" | "transient_network" => return ErrorCategory::TransientNetwork,
            "server_error" | "provider_error" | "provider_5xx" | "upstream_unavailable" => {
                return ErrorCategory::ServerError;
            }
            "auth_failure" => return ErrorCategory::Auth,
            "model_unavailable" => return ErrorCategory::NotFound,
            _ => {}
        }
    }
    if kind == Some("transient") {
        return ErrorCategory::ServerError;
    }
    ErrorCategory::Generic
}

fn default_mock_error_message(
    status: Option<u16>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> String {
    match (status, kind, reason) {
        (Some(status), Some(kind), Some(reason)) => {
            format!("HTTP {status} mock LLM error ({kind}/{reason})")
        }
        (Some(status), _, Some(reason)) => format!("HTTP {status} mock LLM error ({reason})"),
        (Some(status), _, _) => format!("HTTP {status} mock LLM error"),
        (None, Some(kind), Some(reason)) => format!("mock LLM error ({kind}/{reason})"),
        (None, Some(kind), None) => format!("mock LLM error ({kind})"),
        (None, None, Some(reason)) => format!("mock LLM error ({reason})"),
        (None, None, None) => String::new(),
    }
}

#[derive(Clone)]
pub struct LlmMock {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub match_pattern: Option<String>, // None = FIFO (consumed), Some = glob (reusable)
    pub consume_on_match: bool,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub thinking: Option<String>,
    pub thinking_summary: Option<String>,
    pub stop_reason: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub blocks: Option<Vec<serde_json::Value>>,
    pub logprobs: Vec<serde_json::Value>,
    /// When `Some`, this mock synthesizes an error instead of an
    /// `LlmResult`. `text`/`tool_calls` are ignored for error mocks.
    pub error: Option<MockError>,
}

#[derive(Clone)]
pub(crate) struct LlmMockCall {
    pub api_mode: String,
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub provider_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    pub output_format: serde_json::Value,
    pub thinking: serde_json::Value,
    pub previous_response_id: Option<String>,
    pub store: Option<bool>,
    pub background: Option<bool>,
    pub truncation: Option<String>,
    pub compact: Option<bool>,
    pub include: Option<Vec<String>>,
    pub max_tool_calls: Option<i64>,
}

type LlmMockScope = (Vec<LlmMock>, Vec<LlmMockCall>, BTreeSet<String>);

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static TOOL_RECORDINGS: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCKS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static CLI_LLM_MOCK_MODE: RefCell<CliLlmMockMode> = const { RefCell::new(CliLlmMockMode::Off) };
    static CLI_LLM_MOCKS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static CLI_LLM_RECORDINGS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCK_CALLS: RefCell<Vec<LlmMockCall>> = const { RefCell::new(Vec::new()) };
    static LLM_PROMPT_CACHE: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static LLM_MOCK_SCOPES: RefCell<Vec<LlmMockScope>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn push_llm_mock(mock: LlmMock) {
    LLM_MOCKS.with(|v| v.borrow_mut().push(mock));
}

pub(crate) fn get_llm_mock_calls() -> Vec<LlmMockCall> {
    LLM_MOCK_CALLS.with(|v| v.borrow().clone())
}

pub(crate) fn builtin_llm_mock_active() -> bool {
    LLM_MOCKS.with(|v| !v.borrow().is_empty())
}

pub(crate) fn reset_llm_mock_state() {
    LLM_MOCKS.with(|v| v.borrow_mut().clear());
    CLI_LLM_MOCK_MODE.with(|v| *v.borrow_mut() = CliLlmMockMode::Off);
    CLI_LLM_MOCKS.with(|v| v.borrow_mut().clear());
    CLI_LLM_RECORDINGS.with(|v| v.borrow_mut().clear());
    LLM_MOCK_CALLS.with(|v| v.borrow_mut().clear());
    LLM_PROMPT_CACHE.with(|v| v.borrow_mut().clear());
    LLM_MOCK_SCOPES.with(|v| v.borrow_mut().clear());
}

/// Save the current builtin LLM mock queue and recorded-calls list, then
/// start a fresh empty scope. Paired with `pop_llm_mock_scope`. Backs
/// the `with_llm_mocks` helper in `std/testing` so tests reliably
/// roll back to the prior state, including when the body throws.
pub(crate) fn push_llm_mock_scope() {
    let mocks = LLM_MOCKS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let calls = LLM_MOCK_CALLS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let cache = LLM_PROMPT_CACHE.with(|v| std::mem::take(&mut *v.borrow_mut()));
    LLM_MOCK_SCOPES.with(|v| v.borrow_mut().push((mocks, calls, cache)));
}

/// Restore the most recently pushed builtin LLM mock scope. Returns
/// `false` when there is nothing to pop, so the builtin can surface a
/// clear "imbalanced scope" error rather than silently corrupting
/// state. CLI-installed mocks are intentionally untouched: they are an
/// outer harness and should not flicker on each per-test scope swap.
pub(crate) fn pop_llm_mock_scope() -> bool {
    let entry = LLM_MOCK_SCOPES.with(|v| v.borrow_mut().pop());
    match entry {
        Some((mocks, calls, cache)) => {
            LLM_MOCKS.with(|v| *v.borrow_mut() = mocks);
            LLM_MOCK_CALLS.with(|v| *v.borrow_mut() = calls);
            LLM_PROMPT_CACHE.with(|v| *v.borrow_mut() = cache);
            true
        }
        None => false,
    }
}

pub fn clear_cli_llm_mock_mode() {
    CLI_LLM_MOCK_MODE.with(|v| *v.borrow_mut() = CliLlmMockMode::Off);
    CLI_LLM_MOCKS.with(|v| v.borrow_mut().clear());
    CLI_LLM_RECORDINGS.with(|v| v.borrow_mut().clear());
}

pub fn install_cli_llm_mocks(mocks: Vec<LlmMock>) {
    CLI_LLM_MOCK_MODE.with(|v| *v.borrow_mut() = CliLlmMockMode::Replay);
    CLI_LLM_MOCKS.with(|v| *v.borrow_mut() = mocks);
    CLI_LLM_RECORDINGS.with(|v| v.borrow_mut().clear());
}

pub fn enable_cli_llm_mock_recording() {
    CLI_LLM_MOCK_MODE.with(|v| *v.borrow_mut() = CliLlmMockMode::Record);
    CLI_LLM_MOCKS.with(|v| v.borrow_mut().clear());
    CLI_LLM_RECORDINGS.with(|v| v.borrow_mut().clear());
}

pub fn take_cli_llm_recordings() -> Vec<LlmMock> {
    CLI_LLM_RECORDINGS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

pub(crate) fn cli_llm_mock_replay_active() -> bool {
    CLI_LLM_MOCK_MODE.with(|v| *v.borrow() == CliLlmMockMode::Replay)
}

fn record_llm_mock_call(request: &super::api::LlmRequestPayload) {
    LLM_MOCK_CALLS.with(|v| {
        v.borrow_mut().push(LlmMockCall {
            api_mode: request.api_mode.as_str().to_string(),
            messages: request.messages.clone(),
            system: request.system.clone(),
            tools: request.native_tools.clone(),
            provider_tools: if request.provider_tools.is_empty() {
                None
            } else {
                Some(request.provider_tools.clone())
            },
            tool_choice: request.tool_choice.clone(),
            output_format: serde_json::to_value(&request.output_format).unwrap_or_else(|_| {
                serde_json::json!({
                    "kind": "text"
                })
            }),
            thinking: serde_json::to_value(&request.thinking).unwrap_or_else(|_| {
                serde_json::json!({
                    "mode": "disabled"
                })
            }),
            previous_response_id: request.previous_response_id.clone(),
            store: request.store,
            background: request.background,
            truncation: request.truncation.clone(),
            compact: request.compact,
            include: request.include.clone(),
            max_tool_calls: request.max_tool_calls,
        });
    });
}

/// Build an LlmResult from a matched mock.
fn build_mock_result(mock: &LlmMock, last_msg_len: usize) -> LlmResult {
    let (tool_calls, blocks) = if let Some(blocks) = &mock.blocks {
        (mock.tool_calls.clone(), blocks.clone())
    } else {
        let mut blocks = Vec::new();

        if !mock.text.is_empty() {
            blocks.push(serde_json::json!({
                "type": "output_text",
                "text": mock.text,
                "visibility": "public",
            }));
        }

        let mut tool_calls = Vec::new();
        for (i, tc) in mock.tool_calls.iter().enumerate() {
            let id = format!("mock_call_{}", i + 1);
            let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
            let arguments = tc
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            tool_calls.push(serde_json::json!({
                "id": id,
                "type": "tool_call",
                "name": name,
                "arguments": arguments,
            }));
            blocks.push(serde_json::json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "arguments": arguments,
                "visibility": "internal",
            }));
        }

        (tool_calls, blocks)
    };

    LlmResult {
        served_fast: false,
        text: mock.text.clone(),
        tool_calls,
        input_tokens: mock.input_tokens.unwrap_or(last_msg_len as i64),
        output_tokens: mock.output_tokens.unwrap_or(30),
        cache_read_tokens: mock.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: mock.cache_write_tokens.unwrap_or(0),
        cache_supported: true,
        model: mock.model.clone(),
        provider: mock.provider.clone().unwrap_or_else(|| "mock".to_string()),
        thinking: mock.thinking.clone(),
        thinking_summary: mock.thinking_summary.clone(),
        stop_reason: mock.stop_reason.clone(),
        blocks,
        logprobs: mock.logprobs.clone(),
        telemetry: ProviderTelemetry::default(),
    }
}

// Mock prompt patterns match free prose, where `?`/`[`/`{` are ordinary
// characters — only `*` is a wildcard. The shared prose matcher keeps that
// contract (`*`-only ordered segments).
use harn_glob::match_prose as mock_glob_match;

fn collect_mock_match_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => out.push(text.clone()),
        serde_json::Value::String(_) => {}
        serde_json::Value::Array(items) => {
            for item in items {
                collect_mock_match_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_mock_match_strings(value, out);
            }
        }
        _ => {}
    }
}

fn mock_match_text(messages: &[serde_json::Value]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        collect_mock_match_strings(message, &mut parts);
    }
    parts.join("\n")
}

fn mock_last_prompt_text(messages: &[serde_json::Value]) -> String {
    for message in messages.iter().rev() {
        let Some(content) = message.get("content") else {
            continue;
        };
        let mut parts = Vec::new();
        collect_mock_match_strings(content, &mut parts);
        let text = parts.join("\n");
        if !text.trim().is_empty() {
            return text;
        }
    }
    String::new()
}

fn mock_prompt_cache_key(
    model: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
) -> String {
    serde_json::to_string(&serde_json::json!({
        "model": model,
        "system": system,
        "messages": messages,
    }))
    .unwrap_or_default()
}

fn apply_mock_prompt_cache(result: &mut LlmResult, cache_key: &str) {
    if result.cache_read_tokens > 0 || result.cache_write_tokens > 0 {
        return;
    }
    let cache_tokens = result.input_tokens.max(0);
    if cache_tokens == 0 {
        return;
    }
    let cache_hit = LLM_PROMPT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.contains(cache_key) {
            true
        } else {
            cache.insert(cache_key.to_string());
            false
        }
    });
    if cache_hit {
        result.cache_read_tokens = cache_tokens;
    } else {
        result.cache_write_tokens = cache_tokens;
    }
}

/// Convert a mock's `error` payload into the `VmError` that the
/// provider path would have raised, so classification, retry, and
/// `error_category` all behave identically to a real failure.
fn mock_error_to_vm_error(err: &MockError) -> VmError {
    let message = mock_error_message(err);
    if err.has_provider_envelope() {
        let classified = super::api::classify_llm_error(err.category.clone(), &message);
        let mut dict = BTreeMap::new();
        dict.insert(
            "category".to_string(),
            VmValue::String(std::sync::Arc::from(err.category.as_str())),
        );
        dict.insert(
            "kind".to_string(),
            VmValue::String(std::sync::Arc::from(
                err.kind
                    .as_deref()
                    .unwrap_or_else(|| classified.kind.as_str()),
            )),
        );
        dict.insert(
            "reason".to_string(),
            VmValue::String(std::sync::Arc::from(
                err.reason
                    .as_deref()
                    .unwrap_or_else(|| classified.reason.as_str()),
            )),
        );
        dict.insert(
            "message".to_string(),
            VmValue::String(std::sync::Arc::from(message)),
        );
        if let Some(status) = err.status {
            dict.insert("status".to_string(), VmValue::Int(i64::from(status)));
        }
        if let Some(retry_after_ms) = err.retry_after_ms {
            dict.insert(
                "retry_after_ms".to_string(),
                VmValue::Int(retry_after_ms as i64),
            );
        }
        return VmError::Thrown(VmValue::Dict(std::sync::Arc::new(dict)));
    }

    VmError::CategorizedError {
        message,
        category: err.category.clone(),
    }
}

fn mock_error_message(err: &MockError) -> String {
    // Embed legacy category-only retry hints into the message so the
    // same parser that handles live provider headers populates
    // `retry_after_ms` on the final thrown dict.
    let Some(ms) = err.retry_after_ms else {
        return err.message.clone();
    };
    if err.has_provider_envelope() {
        return err.message.clone();
    }
    let secs = (ms as f64 / 1000.0).max(0.0);
    let sep = if err.message.is_empty() || err.message.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{}{sep}retry-after: {secs}\n", err.message)
}

/// Try to find and return a matching mock response. Returns
/// `Some(Ok(LlmResult))` on a text/tool_call match, `Some(Err(VmError))`
/// on an error-mock match, and `None` to fall through to default.
fn try_match_mock_queue(
    mocks: &mut Vec<LlmMock>,
    match_text: &str,
) -> Option<Result<LlmResult, VmError>> {
    if let Some(idx) = mocks.iter().position(|m| m.match_pattern.is_none()) {
        let mock = mocks.remove(idx);
        return Some(match &mock.error {
            Some(err) => Err(mock_error_to_vm_error(err)),
            None => Ok(build_mock_result(&mock, match_text.len())),
        });
    }

    for idx in 0..mocks.len() {
        let mock = &mocks[idx];
        if let Some(ref pattern) = mock.match_pattern {
            if mock_glob_match(pattern, match_text) {
                if mock.consume_on_match {
                    let mock = mocks.remove(idx);
                    return Some(match &mock.error {
                        Some(err) => Err(mock_error_to_vm_error(err)),
                        None => Ok(build_mock_result(&mock, match_text.len())),
                    });
                }
                return Some(match &mock.error {
                    Some(err) => Err(mock_error_to_vm_error(err)),
                    None => Ok(build_mock_result(mock, match_text.len())),
                });
            }
        }
    }

    None
}

fn try_match_builtin_mock(match_text: &str) -> Option<Result<LlmResult, VmError>> {
    LLM_MOCKS.with(|mocks| try_match_mock_queue(&mut mocks.borrow_mut(), match_text))
}

fn try_match_cli_mock(match_text: &str) -> Option<Result<LlmResult, VmError>> {
    CLI_LLM_MOCKS.with(|mocks| try_match_mock_queue(&mut mocks.borrow_mut(), match_text))
}

pub(crate) fn record_cli_llm_result(result: &LlmResult) {
    record_unified_tape_llm_call(result);
    if !CLI_LLM_MOCK_MODE.with(|mode| *mode.borrow() == CliLlmMockMode::Record) {
        return;
    }
    CLI_LLM_RECORDINGS.with(|recordings| {
        recordings.borrow_mut().push(LlmMock {
            text: result.text.clone(),
            tool_calls: result.tool_calls.clone(),
            match_pattern: None,
            consume_on_match: false,
            input_tokens: Some(result.input_tokens),
            output_tokens: Some(result.output_tokens),
            cache_read_tokens: Some(result.cache_read_tokens),
            cache_write_tokens: Some(result.cache_write_tokens),
            thinking: result.thinking.clone(),
            thinking_summary: result.thinking_summary.clone(),
            stop_reason: result.stop_reason.clone(),
            model: result.model.clone(),
            provider: Some(result.provider.clone()),
            blocks: Some(result.blocks.clone()),
            logprobs: result.logprobs.clone(),
            error: None,
        });
    });
}

/// Append an `LlmCall` record to the unified-tape recorder when one is
/// active. The request digest is built from the most recently recorded
/// `LlmMockCall` so the same hashing surface used for fixture matching
/// drives the fidelity oracle's request comparison; falls back to a
/// hash of the response text alone when no matching call is on record
/// (e.g. when `record_llm_mock_call` was bypassed).
fn record_unified_tape_llm_call(result: &LlmResult) {
    if crate::testbench::tape::active_recorder().is_none() {
        return;
    }
    let response_json = serde_json::to_vec(result).unwrap_or_else(|_| Vec::new());
    let request_digest = LLM_MOCK_CALLS
        .with(|calls| calls.borrow().last().cloned())
        .map(|call| {
            let mut request = serde_json::Map::new();
            request.insert("messages".to_string(), serde_json::json!(call.messages));
            request.insert("system".to_string(), serde_json::json!(call.system));
            request.insert("tools".to_string(), serde_json::json!(call.tools));
            request.insert(
                "tool_choice".to_string(),
                serde_json::json!(call.tool_choice),
            );
            request.insert("thinking".to_string(), serde_json::json!(call.thinking));
            request.insert("model".to_string(), serde_json::json!(result.model));
            if call.api_mode != "chat_completions" {
                request.insert("api_mode".to_string(), serde_json::json!(call.api_mode));
            }
            if call.provider_tools.is_some() {
                request.insert(
                    "provider_tools".to_string(),
                    serde_json::json!(call.provider_tools),
                );
            }
            if call
                .output_format
                .get("kind")
                .and_then(|value| value.as_str())
                != Some("text")
            {
                request.insert(
                    "output_format".to_string(),
                    serde_json::json!(call.output_format),
                );
            }
            if call.previous_response_id.is_some() {
                request.insert(
                    "previous_response_id".to_string(),
                    serde_json::json!(call.previous_response_id),
                );
            }
            if call.store.is_some() {
                request.insert("store".to_string(), serde_json::json!(call.store));
            }
            if call.background.is_some() {
                request.insert("background".to_string(), serde_json::json!(call.background));
            }
            if call.truncation.is_some() {
                request.insert("truncation".to_string(), serde_json::json!(call.truncation));
            }
            if call.compact.is_some() {
                request.insert("compact".to_string(), serde_json::json!(call.compact));
            }
            if call.include.is_some() {
                request.insert("include".to_string(), serde_json::json!(call.include));
            }
            if call.max_tool_calls.is_some() {
                request.insert(
                    "max_tool_calls".to_string(),
                    serde_json::json!(call.max_tool_calls),
                );
            }
            let serialized =
                serde_json::to_vec(&serde_json::Value::Object(request)).unwrap_or_default();
            crate::testbench::tape::content_hash(&serialized)
        })
        .unwrap_or_else(|| {
            // Fall back to hashing the response — keeps fidelity comparable
            // across runs even when the request surface wasn't captured.
            crate::testbench::tape::content_hash(result.text.as_bytes())
        });
    crate::testbench::tape::with_active_recorder(|recorder| {
        let response = recorder.payload_from_bytes(response_json);
        Some(crate::testbench::tape::TapeRecordKind::LlmCall {
            request_digest,
            response,
        })
    });
}

fn unmatched_cli_prompt_error(match_text: &str) -> VmError {
    let mut snippet: String = match_text.chars().take(200).collect();
    if match_text.chars().count() > 200 {
        snippet.push_str("...");
    }
    VmError::Runtime(format!("No --llm-mock fixture matched prompt: {snippet:?}"))
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
) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    model.hash(&mut hasher);
    // Canonical JSON hashing is stable across Debug-format changes.
    serde_json::to_string(messages)
        .unwrap_or_default()
        .hash(&mut hasher);
    system.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        "cache_creation_input_tokens": result.cache_write_tokens,
        "model": result.model,
        "provider": result.provider,
        "thinking": result.thinking,
        "thinking_summary": result.thinking_summary,
        "stop_reason": result.stop_reason,
        "blocks": result.blocks,
        "logprobs": result.logprobs,
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
        served_fast: false,
        text: json["text"].as_str().unwrap_or("").to_string(),
        tool_calls: json["tool_calls"].as_array().cloned().unwrap_or_default(),
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

/// Generate stub argument values for required parameters in a tool schema.
/// This makes mock tool calls realistic — a real model would always fill
/// required fields, so the mock should too.
fn mock_required_args(tool_schema: &serde_json::Value) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    // Anthropic: {name, input_schema: {properties, required}}
    // OpenAI:    {function: {name, parameters: {properties, required}}}
    // Harn VM:   {parameters: {name: {type, required}}}  (from tool_define)
    let input_schema = tool_schema
        .get("input_schema")
        .or_else(|| tool_schema.get("inputSchema"))
        .or_else(|| {
            tool_schema
                .get("function")
                .and_then(|f| f.get("parameters"))
        })
        .or_else(|| tool_schema.get("parameters"));
    let Some(schema) = input_schema else {
        return serde_json::Value::Object(args);
    };
    let required: std::collections::BTreeSet<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            if !required.contains(name) {
                continue;
            }
            let ty = prop
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string");
            let placeholder = match ty {
                "integer" => serde_json::json!(0),
                "number" => serde_json::json!(0.0),
                "boolean" => serde_json::json!(false),
                "array" => serde_json::json!([]),
                "object" => serde_json::json!({}),
                _ => serde_json::json!(""),
            };
            args.insert(name.clone(), placeholder);
        }
    }
    serde_json::Value::Object(args)
}

fn mock_tool_name(tool: &serde_json::Value) -> Option<&str> {
    tool.get("name")
        .or_else(|| {
            tool.get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(|name| name.as_str())
}

fn mock_auto_tool_candidate(tools: &[serde_json::Value]) -> Option<&serde_json::Value> {
    tools
        .iter()
        .find(|tool| mock_tool_name(tool) != Some("agent_await_resumption"))
}

/// Mock LLM provider -- deterministic responses for testing without API keys.
/// When configurable mocks have been registered via `llm_mock()`, those are
/// checked first (FIFO queue, then pattern matching). Falls through to the
/// default deterministic behavior when no mocks match.
pub(crate) fn mock_llm_response(
    request: &super::api::LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    record_llm_mock_call(request);

    let messages = &request.messages;
    let system = request.system.as_deref();
    let match_text = mock_match_text(messages);
    let prompt_text = mock_last_prompt_text(messages);
    let cache_key = mock_prompt_cache_key(&request.model, messages, system);

    if let Some(matched) = try_match_cli_mock(&match_text) {
        return matched.map(|mut result| {
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            result
        });
    }

    if let Some(matched) = try_match_builtin_mock(&match_text) {
        return matched.map(|mut result| {
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            result
        });
    }

    if cli_llm_mock_replay_active() {
        return Err(unmatched_cli_prompt_error(&match_text));
    }

    // Generate a mock tool call for the first tool, filling required
    // params with placeholders so the call passes schema validation.
    if let Some(tools) = request.native_tools.as_deref() {
        if let Some(first_tool) = mock_auto_tool_candidate(tools) {
            let tool_name = mock_tool_name(first_tool).unwrap_or("unknown");
            let mock_args = mock_required_args(first_tool);
            let mut result = LlmResult {
                served_fast: false,
                text: String::new(),
                tool_calls: vec![serde_json::json!({
                        "id": "mock_call_1",
                        "type": "tool_call",
                        "name": tool_name,
                "arguments": mock_args
                })],
                input_tokens: prompt_text.len() as i64,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_supported: true,
                model: request.model.clone(),
                provider: "mock".to_string(),
                thinking: None,
                thinking_summary: None,
                stop_reason: None,
                blocks: vec![serde_json::json!({
                    "type": "tool_call",
                    "id": "mock_call_1",
                    "name": tool_name,
                    "arguments": mock_args,
                    "visibility": "internal",
                })],
                logprobs: Vec::new(),
                telemetry: ProviderTelemetry::default(),
            };
            if request.cache {
                apply_mock_prompt_cache(&mut result, &cache_key);
            }
            return Ok(result);
        }
    }

    // Preserve the historical auto-complete behavior for tagged text-tool
    // prompts only. Bare `##DONE##` in no-tool/native prompts changes
    // loop semantics by completing runs that used to exhaust budget unless
    // a fixture explicitly returned the sentinel.
    let tagged_done = system.is_some_and(|s| s.contains("<done>"));

    let prose_body = if prompt_text.is_empty() {
        "Mock LLM response".to_string()
    } else {
        let word_count = prompt_text.split_whitespace().count();
        format!(
            "Mock response to {word_count}-word prompt: {}",
            prompt_text.chars().take(100).collect::<String>()
        )
    };
    let response = if tagged_done {
        format!("<assistant_prose>{prose_body}</assistant_prose>\n<done>##DONE##</done>")
    } else {
        prose_body
    };

    let mut result = LlmResult {
        served_fast: false,
        text: response.clone(),
        tool_calls: vec![],
        input_tokens: prompt_text.len() as i64,
        output_tokens: 30,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: request.model.clone(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": response,
            "visibility": "public",
        })],
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    };
    if request.cache {
        apply_mock_prompt_cache(&mut result, &cache_key);
    }
    Ok(result)
}

/// Take all recorded tool calls, leaving the buffer empty.
pub fn drain_tool_recordings() -> Vec<ToolCallRecord> {
    TOOL_RECORDINGS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}
