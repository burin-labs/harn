use crate::request::LlmRequestPayload;
use serde_json::{json, Value};

/// History shared by synchronous Gemini, Vertex, and Gemini Batch requests.
pub struct GenerateContentHistory {
    pub contents: Vec<Value>,
    pub system_parts: Vec<Value>,
}

pub fn encode_history(opts: &LlmRequestPayload) -> GenerateContentHistory {
    let mut contents = Vec::new();
    let mut system_parts = Vec::new();
    for message in &opts.messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if role == "system" {
            let parts = message_parts(message);
            if !parts.is_empty() {
                system_parts.extend(parts);
            }
            continue;
        }
        let gemini_role = match role {
            "assistant" | "model" => "model",
            _ => "user",
        };
        let parts = message_parts(message);
        if !parts.is_empty() {
            contents.push(json!({"role": gemini_role, "parts": parts}));
        }
    }
    if let Some(system) = opts.system.as_deref().filter(|value| !value.is_empty()) {
        system_parts.insert(0, json!({"text": system}));
    }
    GenerateContentHistory {
        contents,
        system_parts,
    }
}

fn message_parts(message: &Value) -> Vec<Value> {
    if message
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| role == "tool" || role == "tool_result")
    {
        if let Some(part) = function_response_part(message) {
            return vec![part];
        }
    }
    let mut parts = message
        .get("content")
        .map(crate::content::gemini_parts)
        .unwrap_or_default();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            if let Some(part) = function_call_part(call) {
                parts.push(part);
            }
        }
    }
    parts
}

fn function_call_part(call: &Value) -> Option<Value> {
    let function = call.get("function").unwrap_or(call);
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let args = function
        .get("arguments")
        .and_then(|value| {
            value
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .or_else(|| (!value.is_string()).then(|| value.clone()))
        })
        .or_else(|| call.get("arguments").cloned())
        .unwrap_or_else(|| json!({}));
    let mut function_call = json!({"name": name, "args": args});
    if let Some(id) = call
        .get("id")
        .or_else(|| function.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        function_call["id"] = json!(id);
    }
    let mut part = json!({"functionCall": function_call});
    if let Some(signature) = crate::reasoning_history::gemini_tool_call_thought_signature(call) {
        part["thoughtSignature"] = json!(signature);
    }
    Some(part)
}

fn function_response_part(message: &Value) -> Option<Value> {
    let name = message
        .get("name")
        .or_else(|| message.get("tool_name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let response = message
        .get("content")
        .map(function_response_payload)
        .unwrap_or_else(|| json!({}));
    let mut function_response = json!({"name": name, "response": response});
    if let Some(id) = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .or_else(|| message.get("call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        function_response["id"] = json!(id);
    }
    Some(json!({"functionResponse": function_response}))
}

pub(super) fn function_response_payload(content: &Value) -> Value {
    let visible = crate::content::provider_visible_content(content);
    match visible {
        Value::Object(_) => visible,
        Value::String(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(value) => json!({"result": value}),
            Err(_) => json!({"result": text}),
        },
        Value::Null => json!({}),
        other => json!({"result": other}),
    }
}

pub fn tool_config(tool_choice: Option<&Value>) -> Option<Value> {
    let choice = tool_choice?;
    let mode = match choice {
        Value::String(value) => match value.as_str() {
            "none" => "NONE",
            "required" | "any" => "ANY",
            _ => "AUTO",
        },
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("none") => "NONE",
            Some("function" | "tool" | "any" | "required") => "ANY",
            _ => "AUTO",
        },
        _ => "AUTO",
    };
    let mut config = json!({"functionCallingConfig": {"mode": mode}});
    if mode == "ANY" {
        if let Some(name) = choice
            .get("function")
            .and_then(|value| value.get("name"))
            .or_else(|| choice.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            config["functionCallingConfig"]["allowedFunctionNames"] = json!([name]);
        }
    }
    Some(config)
}
