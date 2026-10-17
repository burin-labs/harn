use crate::request::LlmRequestPayload;
use serde_json::{json, Value};

pub const STEP_USER_INPUT: &str = "user_input";
pub const STEP_MODEL_OUTPUT: &str = "model_output";
pub const STEP_THOUGHT: &str = "thought";
pub const STEP_FUNCTION_CALL: &str = "function_call";
pub const STEP_FUNCTION_RESULT: &str = "function_result";

pub struct InteractionsHistory {
    pub steps: Vec<Value>,
    pub system_instruction: Vec<String>,
}

/// Chained requests send only messages after the last assistant turn, since
/// the provider already holds the preceding history under the interaction id.
pub fn encode_history(opts: &LlmRequestPayload) -> InteractionsHistory {
    let mut system_instruction = Vec::new();
    let mut steps = Vec::new();
    let chained = opts
        .previous_response_id
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    let start = if chained {
        opts.messages
            .iter()
            .rposition(|message| matches!(message_role(message), "assistant" | "model"))
            .map(|index| index + 1)
            .unwrap_or(0)
    } else {
        0
    };
    for message in &opts.messages[start.min(opts.messages.len())..] {
        match message_role(message) {
            "system" => {
                let text = super::super::request_text_content(message);
                if !text.is_empty() {
                    system_instruction.push(text);
                }
            }
            "tool" | "tool_result" => {
                if let Some(step) = function_result_step(message) {
                    steps.push(step);
                }
            }
            "assistant" | "model" => push_assistant_steps(message, &mut steps),
            _ => {
                let content = interactions_content(&message["content"]);
                if !content.is_empty() {
                    steps.push(json!({"type": STEP_USER_INPUT, "content": content}));
                }
            }
        }
    }
    InteractionsHistory {
        steps,
        system_instruction,
    }
}

fn message_role(message: &Value) -> &str {
    message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
}

/// A replayed signature precedes the model text and its authorized calls.
fn push_assistant_steps(message: &Value, steps: &mut Vec<Value>) {
    let parts = message
        .get("content")
        .map(crate::content::gemini_parts)
        .unwrap_or_default();
    let mut calls = Vec::new();
    let mut content = Vec::new();
    let mut signature = None;
    let mut remember_signature = |value: &Value| {
        if signature.is_none() {
            signature = crate::reasoning_history::gemini_tool_call_thought_signature(value)
                .map(str::to_string);
        }
    };
    for part in &parts {
        remember_signature(part);
        if let Some(call) = part.get("functionCall") {
            if let Some(step) = function_call_step(call, part) {
                calls.push(step);
            }
            continue;
        }
        if let Some(value) = content_from_gemini_part(part) {
            content.push(value);
        }
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        remember_signature(call);
        if let Some(step) = function_call_step(call.get("function").unwrap_or(call), call) {
            calls.push(step);
        }
    }
    if let Some(signature) = signature {
        steps.push(json!({"type": STEP_THOUGHT, "signature": signature}));
    }
    if !content.is_empty() {
        steps.push(json!({"type": STEP_MODEL_OUTPUT, "content": content}));
    }
    steps.extend(calls);
}

fn function_call_step(call: &Value, owner: &Value) -> Option<Value> {
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let arguments = call
        .get("args")
        .or_else(|| call.get("arguments"))
        .and_then(|value| {
            value
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .or_else(|| (!value.is_string()).then(|| value.clone()))
        })
        .unwrap_or_else(|| json!({}));
    let mut step = json!({"type": STEP_FUNCTION_CALL, "name": name, "arguments": arguments});
    if let Some(id) = call
        .get("id")
        .or_else(|| owner.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        step["id"] = json!(id);
    }
    Some(step)
}

fn function_result_step(message: &Value) -> Option<Value> {
    let name = message
        .get("name")
        .or_else(|| message.get("tool_name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let payload = message
        .get("content")
        .map(super::generate_content::function_response_payload)
        .unwrap_or_else(|| json!({}));
    let text = match &payload {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut step = json!({"type": STEP_FUNCTION_RESULT, "name": name, "result": [{"type": "text", "text": text}]});
    if let Some(id) = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .or_else(|| message.get("call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        step["call_id"] = json!(id);
    }
    Some(step)
}

fn interactions_content(content: &Value) -> Vec<Value> {
    crate::content::gemini_parts(content)
        .iter()
        .filter_map(content_from_gemini_part)
        .collect()
}

fn content_from_gemini_part(part: &Value) -> Option<Value> {
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        if text.is_empty() {
            return None;
        }
        return Some(json!({"type": "text", "text": text}));
    }
    if let Some(inline) = part.get("inline_data").or_else(|| part.get("inlineData")) {
        let mime = media_field(inline, "mime_type", "mimeType")?;
        let data = inline.get("data").and_then(Value::as_str)?;
        return Some(json!({"type": media_kind_for_mime(mime), "data": data, "mime_type": mime}));
    }
    if let Some(file) = part.get("file_data").or_else(|| part.get("fileData")) {
        let mime = media_field(file, "mime_type", "mimeType")?;
        let uri = media_field(file, "file_uri", "fileUri")?;
        return Some(json!({"type": media_kind_for_mime(mime), "uri": uri, "mime_type": mime}));
    }
    None
}

fn media_field<'a>(value: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    value
        .get(snake)
        .or_else(|| value.get(camel))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn media_kind_for_mime(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or_default() {
        "image" => "image",
        "audio" => "audio",
        "video" => "video",
        _ => "document",
    }
}

pub fn tool_choice_config(tool_choice: Option<&Value>) -> Option<Value> {
    let choice = tool_choice?;
    if let Some(name) = named_tool_choice(choice) {
        return Some(json!({"allowed_tools": {"mode": "any", "tools": [name]}}));
    }
    Some(json!(match choice {
        Value::String(value) => match value.as_str() {
            "none" => "none",
            "required" | "any" => "any",
            _ => "auto",
        },
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("none") => "none",
            Some("function" | "tool" | "any" | "required") => "any",
            _ => "auto",
        },
        _ => "auto",
    }))
}

fn named_tool_choice(choice: &Value) -> Option<&str> {
    match choice {
        Value::String(value)
            if !value.is_empty()
                && !matches!(value.as_str(), "auto" | "none" | "any" | "required") =>
        {
            Some(value)
        }
        Value::Object(object)
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some("function" | "tool")
            ) =>
        {
            object
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
        }
        _ => None,
    }
}
