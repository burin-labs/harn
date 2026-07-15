use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::TOOL_PROBE_TOOL_NAME;
use crate::value::VmValue;

pub(crate) fn aggregate_stream_text(text: &str, _provider: &str) -> Value {
    let mut content = String::new();
    let mut calls: BTreeMap<String, PartialStreamCall> = BTreeMap::new();
    let mut frames = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if payload == "[DONE]" {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        collect_stream_content_and_calls(&frame, &mut content, &mut calls);
        frames.push(frame);
    }
    let tool_calls: Vec<Value> = calls
        .into_values()
        .map(|call| {
            json!({
                "id": call.id.unwrap_or_else(|| "stream_tool".to_string()),
                "type": "function",
                "function": {
                    "name": call.name.unwrap_or_default(),
                    "arguments": call.arguments,
                }
            })
        })
        .collect();
    json!({
        "content": content,
        "tool_calls": tool_calls,
        "frames": frames,
    })
}

#[derive(Debug, Default)]
struct PartialStreamCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn collect_stream_content_and_calls(
    frame: &Value,
    content: &mut String,
    calls: &mut BTreeMap<String, PartialStreamCall>,
) {
    collect_anthropic_stream_content_and_calls(frame, content, calls);
    if let Some(text) = frame
        .pointer("/message/content")
        .or_else(|| frame.pointer("/choices/0/delta/content"))
        .or_else(|| frame.pointer("/choices/0/message/content"))
        .or_else(|| frame.get("response"))
        .and_then(Value::as_str)
    {
        content.push_str(text);
    }
    for item in frame
        .pointer("/message/tool_calls")
        .or_else(|| frame.pointer("/choices/0/delta/tool_calls"))
        .or_else(|| frame.pointer("/choices/0/message/tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index.to_string())
            .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| calls.len().to_string());
        let slot = calls.entry(key).or_default();
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            slot.id = Some(id.to_string());
        }
        if let Some(name) = item
            .pointer("/function/name")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
        {
            slot.name = Some(name.to_string());
        }
        if let Some(arguments) = item
            .pointer("/function/arguments")
            .or_else(|| item.get("arguments"))
        {
            match arguments {
                Value::String(delta) => slot.arguments.push_str(delta),
                Value::Object(_) => slot.arguments = arguments.to_string(),
                _ => {}
            }
        }
    }
}

fn collect_anthropic_stream_content_and_calls(
    frame: &Value,
    content: &mut String,
    calls: &mut BTreeMap<String, PartialStreamCall>,
) {
    match frame.get("type").and_then(Value::as_str) {
        Some("content_block_start") => {
            let Some(block) = frame.get("content_block") else {
                return;
            };
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                }
                return;
            }
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                return;
            }
            let key = frame
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index.to_string())
                .unwrap_or_else(|| calls.len().to_string());
            let slot = calls.entry(key).or_default();
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                slot.id = Some(id.to_string());
            }
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                slot.name = Some(name.to_string());
            }
            if let Some(input) = block.get("input").filter(|input| !input.is_null()) {
                if !(input.is_object() && input.as_object().is_some_and(|object| object.is_empty()))
                {
                    slot.arguments = input.to_string();
                }
            }
        }
        Some("content_block_delta") => {
            let key = frame
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index.to_string())
                .unwrap_or_else(|| calls.len().to_string());
            let Some(delta) = frame.get("delta") else {
                return;
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        content.push_str(text);
                    }
                }
                Some("input_json_delta") => {
                    if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                        calls.entry(key).or_default().arguments.push_str(partial);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub(crate) fn probe_tool_registry() -> VmValue {
    let mut value_param = BTreeMap::new();
    value_param.insert("type".to_string(), vm_str("string"));
    value_param.insert(
        "description".to_string(),
        vm_str("The marker value to echo."),
    );
    let mut params = BTreeMap::new();
    params.insert("value".to_string(), VmValue::dict(value_param));
    let tool = vm_dict(&[
        ("name", vm_str(TOOL_PROBE_TOOL_NAME)),
        ("description", vm_str("Echo the probe marker exactly.")),
        ("parameters", VmValue::dict(params)),
    ]);
    vm_dict(&[("tools", VmValue::List(std::sync::Arc::new(vec![tool])))])
}

fn vm_str(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    VmValue::dict(map)
}
