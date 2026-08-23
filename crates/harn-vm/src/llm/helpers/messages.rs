use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::{
    new_transcript_with_events, transcript_asset_list, transcript_id, transcript_message_list,
    transcript_summary_text, vm_value_dict_to_json, vm_value_to_json, TRANSCRIPT_TYPE,
};

pub(crate) fn vm_messages_to_json(msg_list: &[VmValue]) -> Result<Vec<serde_json::Value>, VmError> {
    let mut messages = Vec::new();
    for msg in msg_list {
        if let VmValue::Dict(d) = msg {
            let role = d
                .get("role")
                .map(|v| v.display())
                .unwrap_or_else(|| "user".to_string());
            let content = d
                .get("content")
                .cloned()
                .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from("")));
            let content_json = match &content {
                VmValue::String(text) => serde_json::Value::String(text.to_string()),
                other => vm_value_to_json(other),
            };

            // Preserve the complete provider-neutral message. Provider wire
            // projection belongs exclusively to the selected adapter; doing
            // it here made a transcript valid for only the provider that wrote
            // it and broke model/provider switching on replay.
            let mut message = vm_value_dict_to_json(d);
            if !message
                .get("content")
                .map(|value| !value.is_null())
                .unwrap_or(false)
            {
                message["content"] = content_json;
            }
            if message
                .get("role")
                .and_then(|value| value.as_str())
                .is_none()
            {
                message["role"] = serde_json::json!(role);
            }
            if role == "tool" {
                message["role"] = serde_json::json!("tool_result");
            }
            if matches!(role.as_str(), "tool" | "tool_result")
                && message.get("tool_call_id").is_none()
            {
                if let Some(id) = message
                    .get("tool_use_id")
                    .or_else(|| message.get("call_id"))
                    .cloned()
                {
                    message["tool_call_id"] = id;
                }
            }
            if matches!(role.as_str(), "tool" | "tool_result") {
                if let Some(object) = message.as_object_mut() {
                    object.remove("tool_use_id");
                    object.remove("call_id");
                }
            }
            messages.push(message);
        }
    }
    Ok(messages)
}

pub(crate) fn vm_message(role: &str, content: &str) -> VmValue {
    vm_message_value(role, VmValue::String(arcstr::ArcStr::from(content)))
}

pub(crate) fn vm_message_value(role: &str, content: VmValue) -> VmValue {
    let mut msg = BTreeMap::new();
    msg.put_str("role", role);
    msg.insert("content".to_string(), content);
    VmValue::dict(msg)
}

pub(crate) fn json_messages_to_vm(msg_list: &[serde_json::Value]) -> Vec<VmValue> {
    msg_list
        .iter()
        .filter_map(|msg| {
            let role = msg.get("role").and_then(|v| v.as_str())?;

            // Preserve all fields for tool messages; strict providers
            // (Together AI and others) reject messages missing tool_calls
            // / tool_call_id.
            if role == "tool" || role == "tool_result" || msg.get("tool_calls").is_some() {
                let mut message = msg.clone();
                if role == "tool" {
                    message["role"] = serde_json::json!("tool_result");
                }
                if message.get("tool_call_id").is_none() {
                    if let Some(id) = message
                        .get("tool_use_id")
                        .or_else(|| message.get("call_id"))
                        .cloned()
                    {
                        message["tool_call_id"] = id;
                    }
                }
                if let Some(object) = message.as_object_mut() {
                    object.remove("tool_use_id");
                    object.remove("call_id");
                }
                return Some(crate::stdlib::json_to_vm_value(&message));
            }

            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                return Some(vm_message(role, content));
            }

            if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                if role == "user" {
                    for block in blocks {
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                            let content = block
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let tool_call_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let mut result = BTreeMap::new();
                            result.put_str("role", "tool_result");
                            result.put_str("tool_call_id", tool_call_id);
                            result.put_str("content", content);
                            return Some(VmValue::dict(result));
                        }
                    }
                }
                return Some(vm_message_value(
                    role,
                    crate::stdlib::json_to_vm_value(&serde_json::Value::Array(blocks.clone())),
                ));
            }

            msg.get("content")
                .map(|content| vm_message_value(role, crate::stdlib::json_to_vm_value(content)))
        })
        .collect()
}

pub(crate) fn vm_add_role_message(args: &[VmValue], role: &str) -> Result<VmValue, VmError> {
    match args.first() {
        Some(VmValue::List(list)) => {
            let mut new_messages = (**list).clone();
            new_messages.push(vm_message_value(
                role,
                args.get(1)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from(""))),
            ));
            Ok(VmValue::List(std::sync::Arc::new(new_messages)))
        }
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some(TRANSCRIPT_TYPE) =>
        {
            let mut messages = transcript_message_list(d)?;
            messages.push(vm_message_value(
                role,
                args.get(1)
                    .cloned()
                    .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from(""))),
            ));
            Ok(new_transcript_with_events(
                transcript_id(d),
                messages,
                transcript_summary_text(d),
                d.get("metadata").cloned(),
                Vec::new(),
                transcript_asset_list(d)?,
                d.get("state").and_then(|value| match value {
                    VmValue::String(text) if !text.is_empty() => Some(text.as_ref()),
                    _ => None,
                }),
            ))
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("add_{role}: first argument must be a message list or transcript"),
        )))),
    }
}
