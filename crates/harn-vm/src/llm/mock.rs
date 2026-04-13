use std::cell::RefCell;

use super::api::LlmResult;
use crate::orchestration::ToolCallRecord;

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

// ── Configurable LLM mock responses ─────────────────────────────────

pub(crate) struct LlmMock {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub match_pattern: Option<String>, // None = FIFO (consumed), Some = glob (reusable)
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub thinking: Option<String>,
    pub stop_reason: Option<String>,
    pub model: String,
}

#[derive(Clone)]
pub(crate) struct LlmMockCall {
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub tools: Option<Vec<serde_json::Value>>,
}

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static TOOL_RECORDING_MODE: RefCell<ToolRecordingMode> = const { RefCell::new(ToolRecordingMode::Off) };
    static TOOL_RECORDINGS: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static TOOL_REPLAY_FIXTURES: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCKS: RefCell<Vec<LlmMock>> = const { RefCell::new(Vec::new()) };
    static LLM_MOCK_CALLS: RefCell<Vec<LlmMockCall>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn push_llm_mock(mock: LlmMock) {
    LLM_MOCKS.with(|v| v.borrow_mut().push(mock));
}

pub(crate) fn get_llm_mock_calls() -> Vec<LlmMockCall> {
    LLM_MOCK_CALLS.with(|v| v.borrow().clone())
}

pub(crate) fn reset_llm_mock_state() {
    LLM_MOCKS.with(|v| v.borrow_mut().clear());
    LLM_MOCK_CALLS.with(|v| v.borrow_mut().clear());
}

fn record_llm_mock_call(
    messages: &[serde_json::Value],
    system: Option<&str>,
    native_tools: Option<&[serde_json::Value]>,
) {
    LLM_MOCK_CALLS.with(|v| {
        v.borrow_mut().push(LlmMockCall {
            messages: messages.to_vec(),
            system: system.map(|s| s.to_string()),
            tools: native_tools.map(|t| t.to_vec()),
        });
    });
}

/// Build an LlmResult from a matched mock.
fn build_mock_result(mock: &LlmMock, last_msg_len: usize) -> LlmResult {
    let mut blocks = Vec::new();

    // Add text block if present
    if !mock.text.is_empty() {
        blocks.push(serde_json::json!({
            "type": "output_text",
            "text": mock.text,
            "visibility": "public",
        }));
    }

    // Build tool_calls with auto-generated IDs
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

    LlmResult {
        text: mock.text.clone(),
        tool_calls,
        input_tokens: mock.input_tokens.unwrap_or(last_msg_len as i64),
        output_tokens: mock.output_tokens.unwrap_or(30),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: mock.model.clone(),
        provider: "mock".to_string(),
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

/// Try to find and return a matching mock response.
/// Returns Some(LlmResult) if a mock matched, None to fall through to default.
fn try_match_mock(last_msg: &str) -> Option<LlmResult> {
    LLM_MOCKS.with(|mocks| {
        let mut mocks = mocks.borrow_mut();

        // 1. FIFO: first mock without a match pattern (consumed)
        if let Some(idx) = mocks.iter().position(|m| m.match_pattern.is_none()) {
            let mock = mocks.remove(idx);
            return Some(build_mock_result(&mock, last_msg.len()));
        }

        // 2. Pattern match: scan in reverse (last registered wins)
        for mock in mocks.iter().rev() {
            if let Some(ref pattern) = mock.match_pattern {
                if mock_glob_match(pattern, last_msg) {
                    return Some(build_mock_result(mock, last_msg.len()));
                }
            }
        }

        None
    })
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
    // Use canonical JSON string (not Debug format) for stable hashing
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
) -> LlmResult {
    // Always record the call for inspection via llm_mock_calls().
    record_llm_mock_call(messages, system, native_tools);

    // Extract the last user message for generating a deterministic response.
    let last_msg = messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Check configurable mocks first.
    if let Some(result) = try_match_mock(last_msg) {
        return result;
    }

    // If tools are provided, generate a mock tool call for the first tool.
    // Fill required parameters with placeholder values so mock calls pass
    // schema validation the same way a real model would.
    if let Some(tools) = native_tools {
        if let Some(first_tool) = tools.first() {
            let tool_name = first_tool
                .get("name")
                .or_else(|| first_tool.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            let mock_args = mock_required_args(first_tool);
            return LlmResult {
                text: String::new(),
                tool_calls: vec![serde_json::json!({
                    "id": "mock_call_1",
                    "type": "tool_call",
                    "name": tool_name,
                    "arguments": mock_args
                })],
                input_tokens: last_msg.len() as i64,
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
            };
        }
    }

    // Generate response under the tagged response protocol. Wrap prose in
    // <assistant_prose> and emit a <done> block when the host system prompt
    // advertises the sentinel (agent_loop compatibility).
    let done_block = if system.is_some_and(|s| s.contains("##DONE##")) {
        "\n<done>##DONE##</done>"
    } else {
        ""
    };

    let prose_body = if last_msg.is_empty() {
        "Mock LLM response".to_string()
    } else {
        let word_count = last_msg.split_whitespace().count();
        format!(
            "Mock response to {word_count}-word prompt: {}",
            last_msg.chars().take(100).collect::<String>()
        )
    };
    let response = format!("<assistant_prose>{prose_body}</assistant_prose>{done_block}");

    LlmResult {
        text: response.clone(),
        tool_calls: vec![],
        input_tokens: last_msg.len() as i64,
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
    }
}

// ── Tool recording/replay ────────────────────────────────────────────

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
