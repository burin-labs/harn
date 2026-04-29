use std::cell::RefCell;

use super::api::LlmResult;
use crate::orchestration::ToolCallRecord;
use crate::value::{ErrorCategory, VmError};

/// LLM replay mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LlmReplayMode {
    Off,
    Record,
    Replay,
}

/// Tool recording mode — mirrors LLM replay for tool call results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRecordingMode {
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
    /// Optional hint echoed into the error message as a synthetic
    /// `retry-after:` header so the existing `extract_retry_after_ms`
    /// parser recovers it — matches how real provider errors embed
    /// the value. Lets tests assert that `e.retry_after_ms` flows
    /// end-to-end on the thrown dict.
    pub retry_after_ms: Option<u64>,
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
    pub stop_reason: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub blocks: Option<Vec<serde_json::Value>>,
    /// When `Some`, this mock synthesizes an error instead of an
    /// `LlmResult`. `text`/`tool_calls` are ignored for error mocks.
    pub error: Option<MockError>,
}

#[derive(Clone)]
pub(crate) struct LlmMockCall {
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub thinking: serde_json::Value,
}

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static TOOL_RECORDING_MODE: RefCell<ToolRecordingMode> = const { RefCell::new(ToolRecordingMode::Off) };
    static TOOL_RECORDINGS: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static TOOL_REPLAY_FIXTURES: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCKS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static CLI_LLM_MOCK_MODE: RefCell<CliLlmMockMode> = const { RefCell::new(CliLlmMockMode::Off) };
    static CLI_LLM_MOCKS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static CLI_LLM_RECORDINGS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCK_CALLS: RefCell<Vec<LlmMockCall>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCK_SCOPES: RefCell<Vec<(Vec<LlmMock>, Vec<LlmMockCall>)>> =
        const { RefCell::new(Vec::new()) };
}

pub(crate) fn push_llm_mock(mock: LlmMock) {
    LLM_MOCKS.with(|v| v.borrow_mut().push(mock));
}

pub(crate) fn get_llm_mock_calls() -> Vec<LlmMockCall> {
    LLM_MOCK_CALLS.with(|v| v.borrow().clone())
}

pub(crate) fn reset_llm_mock_state() {
    LLM_MOCKS.with(|v| v.borrow_mut().clear());
    CLI_LLM_MOCK_MODE.with(|v| *v.borrow_mut() = CliLlmMockMode::Off);
    CLI_LLM_MOCKS.with(|v| v.borrow_mut().clear());
    CLI_LLM_RECORDINGS.with(|v| v.borrow_mut().clear());
    LLM_MOCK_CALLS.with(|v| v.borrow_mut().clear());
    LLM_MOCK_SCOPES.with(|v| v.borrow_mut().clear());
}

/// Save the current builtin LLM mock queue and recorded-calls list, then
/// start a fresh empty scope. Paired with `pop_llm_mock_scope`. Backs
/// the `with_llm_mocks` helper in `std/testing` so tests reliably
/// roll back to the prior state, including when the body throws.
pub(crate) fn push_llm_mock_scope() {
    let mocks = LLM_MOCKS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let calls = LLM_MOCK_CALLS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    LLM_MOCK_SCOPES.with(|v| v.borrow_mut().push((mocks, calls)));
}

/// Restore the most recently pushed builtin LLM mock scope. Returns
/// `false` when there is nothing to pop, so the builtin can surface a
/// clear "imbalanced scope" error rather than silently corrupting
/// state. CLI-installed mocks are intentionally untouched: they are an
/// outer harness and should not flicker on each per-test scope swap.
pub(crate) fn pop_llm_mock_scope() -> bool {
    let entry = LLM_MOCK_SCOPES.with(|v| v.borrow_mut().pop());
    match entry {
        Some((mocks, calls)) => {
            LLM_MOCKS.with(|v| *v.borrow_mut() = mocks);
            LLM_MOCK_CALLS.with(|v| *v.borrow_mut() = calls);
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

fn record_llm_mock_call(
    messages: &[serde_json::Value],
    system: Option<&str>,
    native_tools: Option<&[serde_json::Value]>,
    thinking: &super::api::ThinkingConfig,
) {
    LLM_MOCK_CALLS.with(|v| {
        v.borrow_mut().push(LlmMockCall {
            messages: messages.to_vec(),
            system: system.map(|s| s.to_string()),
            tools: native_tools.map(|t| t.to_vec()),
            thinking: serde_json::to_value(thinking).unwrap_or_else(|_| {
                serde_json::json!({
                    "mode": "disabled"
                })
            }),
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
        text: mock.text.clone(),
        tool_calls,
        input_tokens: mock.input_tokens.unwrap_or(last_msg_len as i64),
        output_tokens: mock.output_tokens.unwrap_or(30),
        cache_read_tokens: mock.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: mock.cache_write_tokens.unwrap_or(0),
        model: mock.model.clone(),
        provider: mock.provider.clone().unwrap_or_else(|| "mock".to_string()),
        thinking: mock.thinking.clone(),
        stop_reason: mock.stop_reason.clone(),
        blocks,
    }
}

/// Multi-segment glob match: split on `*` and check segments appear in order.
/// Handles `*`, `prefix*`, `*suffix`, `*contains*`, `pre*mid*suf`, etc.
fn mock_glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if i == parts.len() - 1 {
            if !remaining.ends_with(part) {
                return false;
            }
            remaining = "";
        } else {
            match remaining.find(part) {
                Some(pos) => remaining = &remaining[pos + part.len()..],
                None => return false,
            }
        }
    }
    true
}

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

/// Convert a mock's `error` payload into the `VmError` that the
/// provider path would have raised, so classification, retry, and
/// `error_category` all behave identically to a real failure.
fn mock_error_to_vm_error(err: &MockError) -> VmError {
    // Embed `retry_after_ms` as a synthetic `retry-after:` header on
    // the message so `agent_observe::extract_retry_after_ms` — the
    // same parser that handles real HTTP 429s — surfaces the value
    // on the caller's thrown dict. Keeps the mock path byte-for-byte
    // compatible with a real rate-limit response.
    let message = match err.retry_after_ms {
        Some(ms) => {
            let secs = (ms as f64 / 1000.0).max(0.0);
            let sep = if err.message.is_empty() || err.message.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{}{sep}retry-after: {secs}\n", err.message)
        }
        None => err.message.clone(),
    };
    VmError::CategorizedError {
        message,
        category: err.category.clone(),
    }
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
            stop_reason: result.stop_reason.clone(),
            model: result.model.clone(),
            provider: Some(result.provider.clone()),
            blocks: Some(result.blocks.clone()),
            error: None,
        });
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
        "model": result.model,
        "provider": result.provider,
        "blocks": result.blocks,
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
        text: json["text"].as_str().unwrap_or("").to_string(),
        tool_calls: json["tool_calls"].as_array().cloned().unwrap_or_default(),
        input_tokens: json["input_tokens"].as_i64().unwrap_or(0),
        output_tokens: json["output_tokens"].as_i64().unwrap_or(0),
        cache_read_tokens: json["cache_read_tokens"].as_i64().unwrap_or(0),
        cache_write_tokens: json["cache_write_tokens"].as_i64().unwrap_or(0),
        model: json["model"].as_str().unwrap_or("").to_string(),
        provider: json["provider"].as_str().unwrap_or("mock").to_string(),
        thinking: json["thinking"].as_str().map(|s| s.to_string()),
        stop_reason: json["stop_reason"].as_str().map(|s| s.to_string()),
        blocks: json["blocks"].as_array().cloned().unwrap_or_default(),
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

/// Mock LLM provider -- deterministic responses for testing without API keys.
/// When configurable mocks have been registered via `llm_mock()`, those are
/// checked first (FIFO queue, then pattern matching). Falls through to the
/// default deterministic behavior when no mocks match.
pub(crate) fn mock_llm_response(
    messages: &[serde_json::Value],
    system: Option<&str>,
    native_tools: Option<&[serde_json::Value]>,
    thinking: &super::api::ThinkingConfig,
) -> Result<LlmResult, VmError> {
    record_llm_mock_call(messages, system, native_tools, thinking);

    let match_text = mock_match_text(messages);
    let prompt_text = mock_last_prompt_text(messages);

    if let Some(matched) = try_match_cli_mock(&match_text) {
        return matched;
    }

    if let Some(matched) = try_match_builtin_mock(&match_text) {
        return matched;
    }

    if cli_llm_mock_replay_active() {
        return Err(unmatched_cli_prompt_error(&match_text));
    }

    // Generate a mock tool call for the first tool, filling required
    // params with placeholders so the call passes schema validation.
    if let Some(tools) = native_tools {
        if let Some(first_tool) = tools.first() {
            let tool_name = first_tool
                .get("name")
                .or_else(|| first_tool.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            let mock_args = mock_required_args(first_tool);
            return Ok(LlmResult {
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
                model: "mock".to_string(),
                provider: "mock".to_string(),
                thinking: None,
                stop_reason: None,
                blocks: vec![serde_json::json!({
                    "type": "tool_call",
                    "id": "mock_call_1",
                    "name": tool_name,
                    "arguments": mock_args,
                    "visibility": "internal",
                })],
            });
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

    Ok(LlmResult {
        text: response.clone(),
        tool_calls: vec![],
        input_tokens: prompt_text.len() as i64,
        output_tokens: 30,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        stop_reason: None,
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": response,
            "visibility": "public",
        })],
    })
}

pub fn set_tool_recording_mode(mode: ToolRecordingMode) {
    TOOL_RECORDING_MODE.with(|v| *v.borrow_mut() = mode);
}

pub(crate) fn get_tool_recording_mode() -> ToolRecordingMode {
    TOOL_RECORDING_MODE.with(|v| *v.borrow())
}

/// Append a tool call record during recording mode.
pub(crate) fn record_tool_call(record: ToolCallRecord) {
    TOOL_RECORDINGS.with(|v| v.borrow_mut().push(record));
}

/// Take all recorded tool calls, leaving the buffer empty.
pub fn drain_tool_recordings() -> Vec<ToolCallRecord> {
    TOOL_RECORDINGS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Load tool call fixtures for replay mode.
pub fn load_tool_replay_fixtures(records: Vec<ToolCallRecord>) {
    TOOL_REPLAY_FIXTURES.with(|v| *v.borrow_mut() = records);
}

/// Look up a recorded fixture by tool name + args hash.
pub(crate) fn find_tool_replay_fixture(
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<ToolCallRecord> {
    let hash = crate::orchestration::tool_fixture_hash(tool_name, args);
    TOOL_REPLAY_FIXTURES.with(|v| {
        v.borrow()
            .iter()
            .find(|r| r.tool_name == tool_name && r.args_hash == hash)
            .cloned()
    })
}
