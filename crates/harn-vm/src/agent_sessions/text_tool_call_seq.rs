//! Synthetic ids for text-channel tool calls.
//!
//! A model on the text channel writes an inline `<tool_call>` block that
//! carries no id of its own, so the session mints one (`tc_0`, `tc_1`, …) at
//! dispatch. The counter has to survive a transcript rehydrate, which is why
//! resuming a session re-derives the high-water mark by scanning the messages
//! it is restoring rather than restarting from zero — a restart would reissue
//! an id that an earlier result already answered.

use crate::value::VmValue;

use super::SESSIONS;

pub(crate) fn next_text_tool_call_seq(id: &str) -> Option<u64> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let state = map.get_mut(id)?;
        let seq = state.text_tool_call_seq;
        state.text_tool_call_seq = state.text_tool_call_seq.checked_add(1).unwrap_or(0);
        state.touch();
        Some(seq)
    })
}

fn parse_text_tool_call_seq(id: &str) -> Option<u64> {
    let digits = id.strip_prefix("tc_")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn update_next_text_tool_call_seq(value: &serde_json::Value, next_seq: &mut u64) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                update_next_text_tool_call_seq(item, next_seq);
            }
        }
        serde_json::Value::Object(map) => {
            for key in ["id", "tool_call_id"] {
                if let Some(seq) = map
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_text_tool_call_seq)
                {
                    *next_seq = (*next_seq).max(seq.saturating_add(1));
                }
            }
            for item in map.values() {
                update_next_text_tool_call_seq(item, next_seq);
            }
        }
        _ => {}
    }
}

pub(super) fn next_text_tool_call_seq_from_json_messages(messages: &[serde_json::Value]) -> u64 {
    let mut next_seq = 0;
    for message in messages {
        update_next_text_tool_call_seq(message, &mut next_seq);
    }
    next_seq
}

pub(super) fn next_text_tool_call_seq_from_transcript(transcript: &VmValue) -> u64 {
    let Some(dict) = transcript.as_dict() else {
        return 0;
    };
    let Some(VmValue::List(messages)) = dict.get("messages") else {
        return 0;
    };
    next_text_tool_call_seq_from_json_messages(
        &messages
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect::<Vec<_>>(),
    )
}

thread_local! {
    static FALLBACK_TEXT_TOOL_CALL_SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Next synthetic id for a text tool-call parse.
///
/// Prefers the current session's counter so ids stay unique across the whole
/// transcript. Falls back to a thread-local when there is no session — a raw
/// parse from script context — where uniqueness only has to hold within the
/// call.
pub(crate) fn next_text_tool_call_seq_for_parse() -> u64 {
    if let Some(session_id) = super::current_session_id() {
        if let Some(seq) = next_text_tool_call_seq(&session_id) {
            return seq;
        }
    }
    FALLBACK_TEXT_TOOL_CALL_SEQ.with(|cell| {
        let seq = cell.get();
        cell.set(seq.checked_add(1).unwrap_or(0));
        seq
    })
}
