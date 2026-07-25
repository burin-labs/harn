//! Typed call/result pairing receipts for the LLM transcript sidecar.
//!
//! # Why the transcript needs them
//!
//! Reconstructing "which tool result answers which tool call" from the
//! recorded turns alone does not work on every channel.
//!
//! On the native channel the provider gives each call an id and the result
//! message carries it back, so the pair is visible. On the text channel there
//! is no such id: the model writes an inline `<tool_call>` block, and the
//! result is served back as an ordinary `role: "user"` echo with no identity
//! anywhere on it. A consumer left to pair those by position is guessing —
//! and that is precisely how a generic aggregate placeholder ends up looking
//! like a tool result.
//!
//! `provider_call_response.parsed_tool_calls` cannot stand in either. It is
//! written when the provider replies, before the loop parses the turn, and on
//! a text-channel run it re-parses the same text against its own counter. Its
//! synthetic `tc_N` ids are a parallel id space that no result ever answers.
//!
//! So the two moments that actually know the answer record it: dispatch knows
//! the id a call ran under, and injection knows the message index a result
//! landed at. Together they make the sidecar self-describing on every channel.
//!
//! # Observability-only
//!
//! Like `resolved_dispatch`, nothing recorded here re-enters request
//! construction. The model's next-turn payload is byte-identical with or
//! without these events.

use crate::orchestration::{TOOL_CALL_RECEIPT_VERSION, TOOL_RESULT_RECEIPT_VERSION};
use crate::value::VmValue;

/// Record the calls a batch is about to dispatch, with the ids their results
/// will answer under.
///
/// Emitted before dispatch, so the receipts for one assistant turn appear in
/// the order the calls were requested even when the batch runs them
/// concurrently.
pub(crate) fn emit_tool_call_receipts(calls: &[VmValue]) {
    let Some(session_id) = crate::agent_sessions::current_session_id() else {
        return;
    };
    // The calls were parsed from the message just before the next slot, i.e.
    // the assistant turn this batch answers.
    let Some(next_index) = crate::agent_sessions::next_message_index(&session_id) else {
        return;
    };
    let assistant_message_index = next_index.saturating_sub(1);
    for call in calls {
        let call = crate::llm::helpers::vm_value_to_json(call);
        let mut fields = serde_json::Map::new();
        fields.insert(
            "schema_version".to_string(),
            serde_json::json!(TOOL_CALL_RECEIPT_VERSION),
        );
        fields.insert("session_id".to_string(), serde_json::json!(session_id));
        fields.insert(
            "assistant_message_index".to_string(),
            serde_json::json!(assistant_message_index),
        );
        fields.insert(
            "call_id".to_string(),
            serde_json::json!(string_field(&call, &["id", "tool_call_id"])),
        );
        fields.insert(
            "tool_name".to_string(),
            serde_json::json!(string_field(&call, &["name", "tool_name"])),
        );
        fields.insert(
            "arguments".to_string(),
            call.get("arguments")
                .or_else(|| call.get("tool_args"))
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        );
        crate::llm::append_observability_sidecar_entry("tool_call", fields);
    }
}

/// Record which call the message just injected answers.
///
/// `message_index` is the index [`crate::agent_sessions::inject_message`]
/// returned, which binds the receipt to one exact message instead of to
/// whatever event happens to follow it.
pub(crate) fn emit_tool_result_receipt(
    session_id: &str,
    message_index: usize,
    tool_call_id: &str,
    tool_name: &str,
    ok: bool,
    tool_format: &str,
) {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "schema_version".to_string(),
        serde_json::json!(TOOL_RESULT_RECEIPT_VERSION),
    );
    fields.insert("session_id".to_string(), serde_json::json!(session_id));
    fields.insert(
        "message_index".to_string(),
        serde_json::json!(message_index),
    );
    fields.insert("call_id".to_string(), serde_json::json!(tool_call_id));
    fields.insert("tool_name".to_string(), serde_json::json!(tool_name));
    fields.insert("ok".to_string(), serde_json::json!(ok));
    fields.insert("tool_format".to_string(), serde_json::json!(tool_format));
    crate::llm::append_observability_sidecar_entry("tool_result", fields);
}

fn string_field(call: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| call.get(*key).and_then(serde_json::Value::as_str))
        .unwrap_or_default()
        .to_string()
}
