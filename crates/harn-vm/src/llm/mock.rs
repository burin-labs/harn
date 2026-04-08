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

thread_local! {
    static LLM_REPLAY_MODE: RefCell<LlmReplayMode> = const { RefCell::new(LlmReplayMode::Off) };
    static LLM_FIXTURE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static TOOL_RECORDING_MODE: RefCell<ToolRecordingMode> = const { RefCell::new(ToolRecordingMode::Off) };
    static TOOL_RECORDINGS: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
    static TOOL_REPLAY_FIXTURES: RefCell<Vec<ToolCallRecord>> = const { RefCell::new(Vec::new()) };
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
pub(crate) fn mock_llm_response(
    messages: &[serde_json::Value],
    system: Option<&str>,
    native_tools: Option<&[serde_json::Value]>,
) -> LlmResult {
    // Extract the last user message for generating a deterministic response.
    let last_msg = messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

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

    // Generate response based on the prompt content.
    // Include ##DONE## if the system prompt mentions it (agent_loop compatibility).
    let done_sentinel = if system.is_some_and(|s| s.contains("##DONE##")) {
        " ##DONE##"
    } else {
        ""
    };

    let response = if last_msg.is_empty() {
        format!("Mock LLM response{done_sentinel}")
    } else {
        let word_count = last_msg.split_whitespace().count();
        format!(
            "Mock response to {word_count}-word prompt: {}{done_sentinel}",
            last_msg.chars().take(100).collect::<String>()
        )
    };

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
