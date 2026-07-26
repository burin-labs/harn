//! Transcript history to an OpenAI-legal `messages` array.
//!
//! Every rule here exists because some OpenAI-compatible route rejects, or
//! silently mis-serves, a message sequence Harn can legitimately produce:
//! tool results that must sit adjacent to the call they answer, parallel
//! tool-call batches on single-call routes, orphaned results left behind by
//! compaction, images riding on a `role:"tool"` message, provider-private
//! fields, and reserved `<tool_call>` delimiters.

use std::collections::HashSet;

pub(super) fn enforce_tool_result_adjacency(
    messages: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut cursor = 0;
    while cursor < messages.len() {
        let message = messages[cursor].clone();
        let Some(mut pending_ids) = assistant_tool_call_ids(&message) else {
            normalized.push(message);
            cursor += 1;
            continue;
        };

        normalized.push(message);
        cursor += 1;

        let mut results = Vec::new();
        let mut deferred = Vec::new();
        while cursor < messages.len() && !pending_ids.is_empty() {
            let next = messages[cursor].clone();
            let matching_ids = matching_tool_result_ids(&next, &pending_ids);
            if !matching_ids.is_empty() {
                for id in matching_ids {
                    pending_ids.remove(&id);
                }
                results.push(next);
                cursor += 1;
                continue;
            }
            if is_deferable_non_tool_message(&next) {
                deferred.push(next);
                cursor += 1;
                continue;
            }
            break;
        }

        normalized.extend(results);
        normalized.extend(deferred);
    }
    normalized
}

pub(super) fn drop_orphan_tool_result_messages(
    messages: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut live_tool_call_ids = HashSet::new();
    let mut normalized = Vec::with_capacity(messages.len());
    for message in messages {
        match message.get("role").and_then(|role| role.as_str()) {
            Some("assistant") => {
                if let Some(ids) = assistant_tool_call_ids(&message) {
                    live_tool_call_ids.extend(ids);
                }
                normalized.push(message);
            }
            Some("tool") => match message.get("tool_call_id").and_then(|value| value.as_str()) {
                Some(id) if live_tool_call_ids.remove(id) => normalized.push(message),
                _ => {}
            },
            _ => normalized.push(message),
        }
    }
    normalized
}

pub(super) fn enforce_single_tool_call_history(
    messages: Vec<serde_json::Value>,
    has_native_tools: bool,
) -> Vec<serde_json::Value> {
    if has_native_tools {
        split_parallel_native_tool_call_history(messages)
    } else {
        strip_native_tool_metadata_from_text_history(messages)
    }
}

fn strip_native_tool_metadata_from_text_history(
    messages: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    messages
        .into_iter()
        .map(|mut message| {
            let Some(object) = message.as_object_mut() else {
                return message;
            };
            match object.get("role").and_then(|role| role.as_str()) {
                Some("assistant") => {
                    object.remove("tool_calls");
                }
                Some("tool") => {
                    object.insert(
                        "role".to_string(),
                        serde_json::Value::String("user".to_string()),
                    );
                    object.remove("name");
                    object.remove("tool_call_id");
                }
                _ => {}
            }
            message
        })
        .collect()
}

pub(super) fn split_parallel_native_tool_call_history(
    messages: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut cursor = 0;
    while cursor < messages.len() {
        let message = messages[cursor].clone();
        let Some(tool_calls) = assistant_tool_calls(&message) else {
            normalized.push(message);
            cursor += 1;
            continue;
        };
        if tool_calls.len() <= 1 {
            normalized.push(message);
            cursor += 1;
            continue;
        }

        let ids = tool_calls
            .iter()
            .filter_map(|call| {
                call.get("id")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>();
        cursor += 1;

        let mut results_by_id: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
            std::collections::BTreeMap::new();
        let mut deferred = Vec::new();
        let mut pending_ids = ids.iter().cloned().collect::<HashSet<_>>();
        while cursor < messages.len() && !pending_ids.is_empty() {
            let next = messages[cursor].clone();
            let matching_ids = matching_tool_result_ids(&next, &pending_ids);
            if !matching_ids.is_empty() {
                for id in matching_ids {
                    pending_ids.remove(&id);
                    results_by_id.entry(id).or_default().push(next.clone());
                }
                cursor += 1;
                continue;
            }
            if is_deferable_non_tool_message(&next) {
                deferred.push(next);
                cursor += 1;
                continue;
            }
            break;
        }

        for (idx, call) in tool_calls.into_iter().enumerate() {
            let mut assistant = message.clone();
            // Read the id from this call directly, not by positional index into
            // `ids`: `ids` was compacted with filter_map (calls lacking an id are
            // dropped), so `ids.get(idx)` misaligns once any call has no id and
            // would attach a tool result to the wrong assistant call.
            let call_id = call
                .get("id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            if let Some(object) = assistant.as_object_mut() {
                object.insert(
                    "tool_calls".to_string(),
                    serde_json::Value::Array(vec![call]),
                );
                if idx > 0 {
                    object.insert(
                        "content".to_string(),
                        serde_json::Value::String(String::new()),
                    );
                }
            }
            normalized.push(assistant);
            if let Some(id) = call_id {
                if let Some(results) = results_by_id.remove(&id) {
                    normalized.extend(results);
                }
            }
        }
        normalized.extend(deferred);
    }
    normalized
}

fn assistant_tool_calls(message: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    if message.get("role").and_then(|role| role.as_str()) != Some("assistant") {
        return None;
    }
    let calls = message.get("tool_calls")?.as_array()?.clone();
    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

fn assistant_tool_call_ids(message: &serde_json::Value) -> Option<HashSet<String>> {
    if message.get("role").and_then(|role| role.as_str()) != Some("assistant") {
        return None;
    }
    let ids: HashSet<String> = message
        .get("tool_calls")?
        .as_array()?
        .iter()
        .filter_map(|call| {
            call.get("id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn matching_tool_result_ids(
    message: &serde_json::Value,
    pending_ids: &HashSet<String>,
) -> HashSet<String> {
    if message.get("role").and_then(|role| role.as_str()) != Some("tool") {
        return HashSet::new();
    }
    match message.get("tool_call_id").and_then(|value| value.as_str()) {
        Some(id) if pending_ids.contains(id) => HashSet::from([id.to_string()]),
        _ => HashSet::new(),
    }
}

fn is_deferable_non_tool_message(message: &serde_json::Value) -> bool {
    !matches!(
        message.get("role").and_then(|role| role.as_str()),
        Some("assistant" | "tool")
    )
}

/// Remap canonical `<tool_call>` delimiters to the non-special wire form for a
/// reserved-token model (no-op when `remap` is false). Applied to every outgoing
/// message; see [`crate::llm::tool_delimiter`].
pub(super) fn maybe_remap_tool_call_text(text: &str, remap: bool) -> String {
    if remap {
        crate::llm::tool_delimiter::canonical_to_wire(text)
    } else {
        text.to_string()
    }
}

/// Relocate image parts off `role:"tool"` messages onto a following `role:"user"`
/// message. OpenAI chat-completions rejects image content parts on a tool message
/// (`Image URLs are only allowed for messages with role 'user'`), so when a tool
/// result carries image parts (a computer-use screenshot, or any image-returning
/// tool) the tool message keeps its text parts and the images are carried by a
/// `user` message where OpenAI accepts them.
///
/// The images are NOT inserted inline right after each tool message: OpenAI
/// requires every tool message answering an assistant's `tool_calls` to be
/// contiguous, so an intervening `user` message inside a parallel tool-call batch
/// (`assistant[c1,c2]`, `tool(c1 image)`, `tool(c2)`) would be a 400. Instead the
/// stripped images are buffered across the contiguous run of tool results and
/// flushed as a single `user` message once the run ends (the next non-tool
/// message, or the end of history). Messages with no tool-message image content
/// pass through untouched.
pub(super) fn relocate_tool_message_images_to_user(
    msgs: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    fn is_image_part(part: &serde_json::Value) -> bool {
        matches!(
            part.get("type").and_then(|value| value.as_str()),
            Some("image_url") | Some("image")
        )
    }
    // Emit the buffered images (if any) as one `user` message; called when a
    // tool-result run ends so the images land after the whole run, never between
    // two tool messages.
    fn flush(out: &mut Vec<serde_json::Value>, pending: &mut Vec<serde_json::Value>) {
        if pending.is_empty() {
            return;
        }
        let mut user_content = vec![serde_json::json!({
            "type": "text",
            "text": "Screenshot(s) from the preceding tool result(s):",
        })];
        user_content.append(pending);
        out.push(serde_json::json!({"role": "user", "content": user_content}));
    }
    let mut out = Vec::with_capacity(msgs.len() + 1);
    let mut pending_images: Vec<serde_json::Value> = Vec::new();
    for message in msgs {
        let is_tool = message.get("role").and_then(|value| value.as_str()) == Some("tool");
        if !is_tool {
            // The tool-result run (if any) ended; land the buffered images just
            // before this non-tool message.
            flush(&mut out, &mut pending_images);
            out.push(message);
            continue;
        }
        let parts = message.get("content").and_then(|value| value.as_array());
        let Some(parts) = parts else {
            out.push(message);
            continue;
        };
        if !parts.iter().any(is_image_part) {
            out.push(message);
            continue;
        }
        let (images, text_parts): (Vec<_>, Vec<_>) = parts.iter().cloned().partition(is_image_part);
        // Keep the tool message with its text parts. OpenAI requires non-empty
        // content, so fall back to a short note when the result was image-only.
        let mut tool_message = message.clone();
        let tool_content = if text_parts.is_empty() {
            serde_json::json!("(screenshot returned; see the image in the following message)")
        } else {
            serde_json::Value::Array(text_parts)
        };
        if let Some(object) = tool_message.as_object_mut() {
            object.insert("content".to_string(), tool_content);
        }
        out.push(tool_message);
        pending_images.extend(images);
    }
    // Flush images from a tool-result run that ran to the end of history.
    flush(&mut out, &mut pending_images);
    out
}

fn remap_tool_call_content(content: &serde_json::Value) -> serde_json::Value {
    use crate::llm::tool_delimiter::canonical_to_wire;
    match content {
        serde_json::Value::String(s) => serde_json::Value::String(canonical_to_wire(s)),
        serde_json::Value::Array(parts) => serde_json::Value::Array(
            parts
                .iter()
                .map(|part| {
                    let mut part = part.clone();
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        let remapped = canonical_to_wire(text);
                        if let Some(obj) = part.as_object_mut() {
                            obj.insert("text".to_string(), serde_json::Value::String(remapped));
                        }
                    }
                    part
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn sanitize_openai_message_for_request(
    message: &mut serde_json::Value,
    remap_tool_call: bool,
    reasoning_history_wire_field: Option<crate::llm::capabilities::ReasoningHistoryWireField>,
) {
    let Some(object) = message.as_object_mut() else {
        return;
    };
    let role = object
        .get("role")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    // Harn stores private reasoning under its canonical `reasoning` key and
    // strips every provider-private field by default. A small number of routes
    // reject a valid tool-result follow-up unless their exact prior assistant
    // reasoning field is replayed. Project only Harn-owned canonical reasoning
    // through the typed catalog mode; never pass through arbitrary seeded
    // `reasoning_content` from transcript/history input.
    let replay_reasoning = (role.as_deref() == Some("assistant"))
        .then(|| object.get("reasoning").and_then(serde_json::Value::as_str))
        .flatten()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    object.retain(|key, _| openai_message_key_allowed(role.as_deref(), key));

    if let (Some(field), Some(reasoning)) = (reasoning_history_wire_field, replay_reasoning) {
        object.insert(
            field.as_str().to_string(),
            serde_json::Value::String(reasoning),
        );
    }

    if let Some(content) = object.get("content").cloned() {
        let content = if remap_tool_call {
            remap_tool_call_content(&content)
        } else {
            content
        };
        object.insert(
            "content".to_string(),
            crate::llm::content::openai_content(&content),
        );
    }
    if let Some(tool_calls) = object.get_mut("tool_calls") {
        sanitize_openai_tool_calls_for_request(tool_calls);
    }
}

fn openai_message_key_allowed(role: Option<&str>, key: &str) -> bool {
    matches!(key, "role" | "content" | "name")
        || (key == "tool_calls" && role == Some("assistant"))
        || (key == "tool_call_id" && role == Some("tool"))
}

fn sanitize_openai_tool_calls_for_request(tool_calls: &mut serde_json::Value) {
    let Some(calls) = tool_calls.as_array_mut() else {
        return;
    };
    for call in calls {
        *call = normalize_openai_tool_call_for_request(call);
    }
}

fn normalize_openai_tool_call_for_request(call: &serde_json::Value) -> serde_json::Value {
    let Some(object) = call.as_object() else {
        return call.clone();
    };

    let mut normalized = serde_json::Map::new();
    if let Some(id) = object.get("id").cloned() {
        normalized.insert("id".to_string(), id);
    }
    normalized.insert(
        "type".to_string(),
        serde_json::Value::String("function".to_string()),
    );

    let function = object
        .get("function")
        .and_then(serde_json::Value::as_object);
    let name = function
        .and_then(|function| function.get("name"))
        .or_else(|| object.get("name"));
    let arguments = function
        .and_then(|function| function.get("arguments"))
        .or_else(|| object.get("arguments"));

    if name.is_some() || arguments.is_some() {
        let mut normalized_function = serde_json::Map::new();
        if let Some(name) = name.cloned() {
            normalized_function.insert("name".to_string(), name);
        }
        if let Some(arguments) = arguments {
            normalized_function.insert(
                "arguments".to_string(),
                openai_tool_arguments_string(arguments),
            );
        }
        normalized.insert(
            "function".to_string(),
            serde_json::Value::Object(normalized_function),
        );
    }

    serde_json::Value::Object(normalized)
}

fn openai_tool_arguments_string(arguments: &serde_json::Value) -> serde_json::Value {
    match arguments {
        serde_json::Value::String(_) => arguments.clone(),
        serde_json::Value::Null => serde_json::Value::String("{}".to_string()),
        other => serde_json::Value::String(
            serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
        ),
    }
}
