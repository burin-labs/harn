use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::{
    new_transcript_with_events, transcript_asset_list, transcript_id, transcript_message_list,
    transcript_summary_text, vm_value_dict_to_json, vm_value_to_json, TRANSCRIPT_TYPE,
};

/// Provider-neutral JSON projection of VM messages.
///
/// Every call re-sends the whole transcript, and the messages it carries over
/// from the previous call are the same shared VM values, so each message's
/// projection is memoized by identity and only messages new at their position
/// are converted. Non-dict entries are skipped.
pub(crate) fn vm_messages_to_json(msg_list: &[VmValue]) -> Result<Vec<serde_json::Value>, VmError> {
    Ok(
        crate::llm::content_hash::memoized_json_messages(msg_list, vm_message_to_json)
            .into_iter()
            .flatten()
            .collect(),
    )
}

fn vm_message_to_json(msg: &VmValue) -> Option<serde_json::Value> {
    let VmValue::Dict(d) = msg else {
        return None;
    };
    let role = d
        .get("role")
        .map(|v| v.display())
        .unwrap_or_else(|| "user".to_string());
    let content = d
        .get("content")
        .cloned()
        .unwrap_or_else(|| VmValue::string(""));
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
    let tool_result = harn_orchestration::tool_messages::is_tool_result_role(&role);
    if tool_result && message.get("tool_call_id").is_none() {
        if let Some(id) = message
            .get("tool_use_id")
            .or_else(|| message.get("call_id"))
            .cloned()
        {
            message["tool_call_id"] = id;
        }
    }
    if tool_result {
        if let Some(object) = message.as_object_mut() {
            object.remove("tool_use_id");
            object.remove("call_id");
        }
    }
    Some(message)
}

pub(crate) fn vm_message(role: &str, content: &str) -> VmValue {
    vm_message_value(role, VmValue::string(content))
}

pub(crate) fn vm_message_value(role: &str, content: VmValue) -> VmValue {
    let mut msg = BTreeMap::new();
    msg.put_str("role", role);
    msg.insert("content".to_string(), content);
    VmValue::dict(msg)
}

/// VM projection of provider-neutral JSON messages.
///
/// Every call re-projects the whole request history, so each message's
/// projection is memoized by content and only messages new to this thread are
/// converted. The projection is a pure function of the message.
pub(crate) fn json_messages_to_vm(msg_list: &[serde_json::Value]) -> Vec<VmValue> {
    crate::llm::content_hash::memoized_vm_messages(msg_list, json_message_to_vm)
        .into_iter()
        .flatten()
        .collect()
}

fn json_message_to_vm(msg: &serde_json::Value) -> Option<VmValue> {
    let role = msg.get("role").and_then(|v| v.as_str())?;

    // Preserve all fields for tool messages; strict providers
    // (Together AI and others) reject messages missing tool_calls
    // / tool_call_id.
    if harn_orchestration::tool_messages::is_tool_result_role(role)
        || msg.get("tool_calls").is_some()
    {
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
        return Some(crate::schema::json_to_vm_value(&message));
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
            crate::schema::json_to_vm_value(&serde_json::Value::Array(blocks.clone())),
        ));
    }

    msg.get("content")
        .map(|content| vm_message_value(role, crate::schema::json_to_vm_value(content)))
}

pub(crate) fn vm_add_role_message(args: &[VmValue], role: &str) -> Result<VmValue, VmError> {
    match args.first() {
        Some(VmValue::List(list)) => {
            let mut new_messages = (**list).clone();
            new_messages.push(vm_message_value(
                role,
                args.get(1).cloned().unwrap_or_else(|| VmValue::string("")),
            ));
            Ok(VmValue::list(new_messages))
        }
        Some(VmValue::Dict(d))
            if d.get("_type").map(|v| v.display()).as_deref() == Some(TRANSCRIPT_TYPE) =>
        {
            let mut messages = transcript_message_list(d)?;
            messages.push(vm_message_value(
                role,
                args.get(1).cloned().unwrap_or_else(|| VmValue::string("")),
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
        _ => Err(VmError::Thrown(VmValue::string(format!(
            "add_{role}: first argument must be a message list or transcript"
        )))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(len: usize) -> Vec<serde_json::Value> {
        (0..len)
            .map(|index| {
                if index % 2 == 0 {
                    serde_json::json!({"role": "user", "content": format!("read file {index}")})
                } else {
                    serde_json::json!({
                        "role": "tool",
                        "tool_use_id": format!("call-{index}"),
                        "content": [{"type": "text", "text": format!("body {index}")}],
                    })
                }
            })
            .collect()
    }

    /// Re-projecting a growing history converts only the appended message.
    /// Converting the whole history again every call is the quadratic cost
    /// this guards against.
    #[test]
    fn json_messages_to_vm_converts_only_new_messages() {
        let tag = uuid::Uuid::now_v7().to_string();
        let mut messages = history(6);
        for message in &mut messages {
            message["tag"] = serde_json::json!(tag);
        }
        let before = crate::llm::content_hash::vm_message_projections();
        let first = json_messages_to_vm(&messages);
        assert_eq!(
            crate::llm::content_hash::vm_message_projections() - before,
            messages.len()
        );

        messages.push(serde_json::json!({"role": "assistant", "content": "done", "tag": tag}));
        let before = crate::llm::content_hash::vm_message_projections();
        let second = json_messages_to_vm(&messages);
        assert_eq!(
            crate::llm::content_hash::vm_message_projections() - before,
            1,
            "only the appended message is converted"
        );

        for (index, (memoized, message)) in second.iter().zip(&messages).enumerate() {
            let fresh = json_message_to_vm(message).expect("message has a role");
            assert!(
                crate::values_equal(memoized, &fresh),
                "memoized projection {index} differs from a fresh conversion"
            );
            if let Some(earlier) = first.get(index) {
                assert!(crate::values_equal(memoized, earlier));
            }
        }
    }

    /// Building each call's options converts the carried-over transcript,
    /// which is the same shared VM values as on the previous call, so only
    /// the messages appended since then are converted to JSON. Converting the
    /// whole transcript every call is the quadratic cost this guards against.
    #[test]
    fn vm_messages_to_json_converts_only_new_messages() {
        let tag = uuid::Uuid::now_v7().to_string();
        let mut transcript = Vec::new();
        let mut converted = 0;
        let turns = 12;
        for turn in 0..turns {
            let mut appended = history(2);
            for message in &mut appended {
                message["tag"] = serde_json::json!(format!("{tag}-{turn}"));
            }
            transcript.extend(json_messages_to_vm(&appended));
            let before = crate::llm::content_hash::json_message_projections();
            let json = vm_messages_to_json(&transcript).expect("message json");
            converted += crate::llm::content_hash::json_message_projections() - before;

            assert_eq!(json.len(), transcript.len());
            for (index, (memoized, message)) in json.iter().zip(&transcript).enumerate() {
                assert_eq!(
                    Some(memoized),
                    vm_message_to_json(message).as_ref(),
                    "memoized projection {index} differs from a fresh conversion"
                );
            }
        }
        // Every message is new once, so each was converted at least once and
        // the counter is measuring the path under test.
        assert!(converted >= transcript.len());
        // Converting the whole transcript each call costs about
        // turns * messages / 2 (here 156).
        assert!(
            converted <= transcript.len(),
            "{converted} conversions for {} messages over {turns} calls: \
             the transcript is being converted on every call",
            transcript.len()
        );
    }
}
