//! Provider-valid tool-call history, repaired once for every route.
//!
//! Every native tool-calling API pairs each assistant tool call with a result
//! that follows it directly, and rejects a result whose call is not the
//! assistant turn it answers. Durable history breaks both rules in ordinary
//! operation: a stopped turn leaves a call unanswered (harn#8951), runtime
//! feedback lands between a call and its result, and compaction or trimming
//! removes a call while keeping its result. [`repair_tool_call_history`]
//! restores the invariant on the send-safe request payload before any provider
//! dialect lowers it. The stored transcript is never rewritten, so a history
//! written before a repair existed heals on its next request.
//!
//! The pass reads Harn's canonical shapes (assistant `tool_calls`, `tool_result`
//! messages) and the provider-native aliases that replayed or seeded
//! transcripts carry: the OpenAI `tool` role, Anthropic `tool_use` and
//! `tool_result` blocks, Gemini `functionCall` and `functionResponse` parts, and
//! Responses `function_call_output` items.
//!
//! The session layer keeps a separate repair, `pair_orphaned_tool_use`, because
//! it has different semantics: it writes the harness's corrective feedback into
//! the durable transcript as the result of calls the loop declined to dispatch.
//! This pass only makes whatever history arrives sendable.

use std::collections::HashSet;

/// What the model reads for a tool call no result answered (harn#8951).
pub(crate) const UNANSWERED_TOOL_CALL_OBSERVATION: &str =
    "No result: the turn was stopped before this tool call finished.";

/// Prefix on a result kept as an observation after its call was removed.
pub(crate) const UNPAIRED_TOOL_RESULT_MARKER: &str = "[unpaired durable tool result]";

/// Make a conversation's tool-call history valid for every provider.
///
/// For each assistant turn that issues native calls:
///
/// - results for those calls are moved directly after the turn, and messages
///   interleaved between the call and its results (runtime feedback, system
///   directives, blank assistant turns) move after the results;
/// - every call still unanswered gets the harness's typed repair result, after
///   the real results and before the deferred messages;
/// - a result that answers no open call of the preceding assistant turn becomes
///   an observation, so its evidence survives without an invalid pairing.
///
/// Calls without an id are left alone: no provider can pair a result to them.
pub(crate) fn repair_tool_call_history(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut messages = messages.into_iter().peekable();
    while let Some(message) = messages.next() {
        let Turn::Calls(calls) = Turn::of(&message) else {
            repaired.push(settle_results(message, &mut HashSet::new()));
            continue;
        };
        repaired.push(message);
        let mut open: HashSet<String> = calls.iter().map(|call| call.id.clone()).collect();
        let mut deferred = Vec::new();
        while !open.is_empty() {
            let Some(next) = messages.peek() else {
                break;
            };
            match Turn::of(next) {
                Turn::Results(ids) if ids.iter().any(|id| open.contains(id)) => {
                    let next = messages.next().expect("peeked message");
                    repaired.push(settle_results(next, &mut open));
                }
                // A result answering no open call is an orphan wherever it
                // sits, so it waits with the other deferred messages instead
                // of closing the window before a real result arrives.
                Turn::Results(_) | Turn::Transparent => {
                    deferred.push(messages.next().expect("peeked message"));
                }
                Turn::Calls(_) | Turn::Boundary => break,
            }
        }
        repaired.extend(
            calls
                .iter()
                .filter(|call| open.contains(&call.id))
                .map(|call| {
                    crate::llm::agent_session_host::harness_repair_tool_result(
                        &call.name,
                        &call.id,
                        UNANSWERED_TOOL_CALL_OBSERVATION,
                    )
                }),
        );
        repaired.extend(
            deferred
                .into_iter()
                .map(|message| settle_results(message, &mut HashSet::new())),
        );
    }
    repaired
}

/// One native call an assistant turn issues.
struct NativeCall {
    id: String,
    name: String,
}

/// How a message takes part in tool-call pairing.
enum Turn {
    /// An assistant turn issuing calls that carry ids, in declaration order.
    Calls(Vec<NativeCall>),
    /// A message answering calls, by id. An id-less result answers `""`.
    Results(Vec<String>),
    /// A message that may sit after a call's results without changing what
    /// the conversation means: user, system, and developer turns without
    /// results, and assistant turns with nothing visible.
    Transparent,
    /// An assistant turn that closes the preceding call's result window.
    Boundary,
}

impl Turn {
    fn of(message: &serde_json::Value) -> Self {
        match role(message) {
            role if harn_orchestration::tool_messages::is_tool_result_role(role) => {
                Self::Results(vec![result_role_id(message).to_string()])
            }
            "assistant" => {
                let calls = assistant_calls(message);
                if !calls.is_empty() {
                    Self::Calls(calls)
                } else if is_blank_assistant(message) {
                    Self::Transparent
                } else {
                    Self::Boundary
                }
            }
            _ => {
                let ids: Vec<String> = content_blocks(message)
                    .filter_map(block_result_id)
                    .map(str::to_string)
                    .collect();
                if ids.is_empty() {
                    Self::Transparent
                } else {
                    Self::Results(ids)
                }
            }
        }
    }
}

fn role(message: &serde_json::Value) -> &str {
    message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn content_blocks(message: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    message
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
}

fn str_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
}

fn assistant_calls(message: &serde_json::Value) -> Vec<NativeCall> {
    let canonical = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| {
            let name = str_field(call, "name")
                .or_else(|| call.get("function").and_then(|f| str_field(f, "name")));
            (str_field(call, "id"), name)
        });
    let blocks = content_blocks(message).filter_map(|block| {
        if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use") {
            return Some((str_field(block, "id"), str_field(block, "name")));
        }
        let call = block.get("functionCall")?;
        Some((str_field(call, "id"), str_field(call, "name")))
    });
    canonical
        .chain(blocks)
        .filter_map(|(id, name)| {
            Some(NativeCall {
                id: id?.to_string(),
                name: name.unwrap_or_default().to_string(),
            })
        })
        .collect()
}

fn is_blank_assistant(message: &serde_json::Value) -> bool {
    match message.get("content") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(text)) => text.trim().is_empty(),
        Some(serde_json::Value::Array(blocks)) => blocks.iter().all(|block| {
            block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|text| text.trim().is_empty())
        }),
        Some(_) => false,
    }
}

fn result_role_id(message: &serde_json::Value) -> &str {
    ["tool_call_id", "tool_use_id", "call_id"]
        .into_iter()
        .find_map(|key| message.get(key).and_then(serde_json::Value::as_str))
        .unwrap_or_default()
}

/// The call id a content block answers, when the block is a result.
fn block_result_id(block: &serde_json::Value) -> Option<&str> {
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("tool_result") => Some(
            block
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        ),
        Some("function_call_output") => Some(
            block
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        ),
        _ => block.get("functionResponse").map(|response| {
            response
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        }),
    }
}

/// Pair each result in `message` with an `open` call, closing that call, and
/// turn every result that pairs with none into an observation.
fn settle_results(mut message: serde_json::Value, open: &mut HashSet<String>) -> serde_json::Value {
    if harn_orchestration::tool_messages::is_tool_result_role(role(&message)) {
        if open.remove(result_role_id(&message)) {
            return message;
        }
        let content = message.get("content").cloned().unwrap_or_default();
        return serde_json::json!({"role": "user", "content": unpaired_observation(content)});
    }
    if role(&message) == "assistant" {
        return message;
    }
    let Some(blocks) = message
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return message;
    };
    let mut settled = Vec::with_capacity(blocks.len());
    for block in std::mem::take(blocks) {
        match block_result_id(&block) {
            Some(id) if !open.remove(id) => settled.extend(unpaired_block_parts(&block)),
            _ => settled.push(block),
        }
    }
    *blocks = settled;
    message
}

/// The content blocks that keep an unpaired result block's evidence. A
/// structured result keeps its own parts, so the visibility and cache
/// metadata they carry still govern how each provider lowers them.
fn unpaired_block_parts(block: &serde_json::Value) -> Vec<serde_json::Value> {
    let payload = block
        .get("content")
        .or_else(|| block.get("output"))
        .or_else(|| {
            block
                .get("functionResponse")
                .and_then(|r| r.get("response"))
        })
        .cloned()
        .unwrap_or_default();
    match unpaired_observation(payload) {
        serde_json::Value::Array(parts) => parts,
        serde_json::Value::String(text) => vec![serde_json::json!({"type": "text", "text": text})],
        other => vec![serde_json::json!({"type": "text", "text": other.to_string()})],
    }
}

fn unpaired_observation(content: serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(mut parts) => {
            parts.insert(
                0,
                serde_json::json!({"type": "text", "text": UNPAIRED_TOOL_RESULT_MARKER}),
            );
            serde_json::Value::Array(parts)
        }
        serde_json::Value::String(text) => {
            serde_json::json!(format!("{UNPAIRED_TOOL_RESULT_MARKER}\n{text}"))
        }
        other => serde_json::json!(format!("{UNPAIRED_TOOL_RESULT_MARKER}\n{other}")),
    }
}

/// Every tool result in a request's messages, in order, with the text the
/// model reads and the tool that produced it. Reads the same shapes the
/// repair does, so a run tape reader never has to.
pub(crate) fn seen_tool_results(
    messages: &[serde_json::Value],
) -> Vec<crate::testbench::tape::SeenToolResult> {
    let mut tools_by_call = std::collections::HashMap::new();
    let mut seen = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if role(message) == "assistant" {
            for call in assistant_calls(message) {
                tools_by_call.insert(call.id, call.name);
            }
            continue;
        }
        let mut results = Vec::new();
        if harn_orchestration::tool_messages::is_tool_result_role(role(message)) {
            let tool = str_field(message, "name").or_else(|| str_field(message, "tool_name"));
            results.push((result_role_id(message), tool, message.get("content")));
        } else {
            for block in content_blocks(message) {
                let Some(id) = block_result_id(block) else {
                    continue;
                };
                let response = block.get("functionResponse");
                let payload = block
                    .get("content")
                    .or_else(|| block.get("output"))
                    .or_else(|| response.and_then(|r| r.get("response")));
                results.push((id, response.and_then(|r| str_field(r, "name")), payload));
            }
        }
        for (id, tool, payload) in results {
            let id = (!id.is_empty()).then(|| id.to_string());
            let tool = tool
                .map(str::to_string)
                .or_else(|| id.as_ref().and_then(|id| tools_by_call.get(id).cloned()))
                .filter(|tool: &String| !tool.is_empty());
            seen.push(crate::testbench::tape::SeenToolResult {
                message: index as u64,
                tool_call_id: id,
                tool,
                text: payload.map(result_text).unwrap_or_default(),
            });
        }
    }
    seen
}

/// A result payload as text: strings verbatim, text parts joined in order,
/// anything else as JSON.
fn result_text(payload: &serde_json::Value) -> String {
    match payload {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .map(|part| match part.get("text") {
                Some(serde_json::Value::String(text)) => text.clone(),
                _ => result_text(part),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "tool_call_history_tests.rs"]
mod tests;
