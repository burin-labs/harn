//! Transcript introspection builtins. Counts message shapes without scraping
//! logs, enabling pipeline tests to assert transcript continuity invariants
//! across stage transitions.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;
use crate::vm::Vm;

pub(crate) fn register_transcript_builtins(vm: &mut Vm) {
    vm.register_builtin("transcript_stats", |args, _out| {
        let transcript = args.first().cloned().unwrap_or(VmValue::Nil);
        let stats = compute_transcript_stats(&transcript);
        Ok(stats_to_vm(&stats))
    });

    vm.register_builtin("transcript_events_by_kind", |args, _out| {
        let transcript = args.first().cloned().unwrap_or(VmValue::Nil);
        let kind = args.get(1).map(|v| v.display()).unwrap_or_default();
        let events = events_list(&transcript);
        let filtered: Vec<VmValue> = events
            .into_iter()
            .filter(|event| match event {
                VmValue::Dict(dict) => dict
                    .get("kind")
                    .and_then(|v| match v {
                        VmValue::String(s) => Some(s.as_ref()),
                        _ => None,
                    })
                    .map(|s| s == kind)
                    .unwrap_or(false),
                _ => false,
            })
            .collect();
        Ok(VmValue::List(Rc::new(filtered)))
    });
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptStats {
    pub message_count: i64,
    pub user_facing_message_count: i64,
    pub tool_result_message_count: i64,
    pub tool_call_count: i64,
    pub assistant_message_count: i64,
    pub user_message_count: i64,
    pub visible_event_count: i64,
}

pub(crate) fn compute_transcript_stats(transcript: &VmValue) -> TranscriptStats {
    let Some(dict) = transcript.as_dict() else {
        return TranscriptStats::default();
    };
    let messages = list_field(dict, "messages");
    let events = list_field(dict, "events");

    let mut stats = TranscriptStats::default();
    for message in &messages {
        let Some(msg) = message.as_dict() else {
            continue;
        };
        stats.message_count += 1;
        let role = string_field(msg, "role").unwrap_or_default();
        match role.as_str() {
            "assistant" => {
                stats.assistant_message_count += 1;
                stats.user_facing_message_count += 1;
            }
            "user" => {
                stats.user_message_count += 1;
                stats.user_facing_message_count += 1;
            }
            "tool" | "tool_result" => {
                stats.tool_result_message_count += 1;
            }
            _ => {}
        }
        stats.tool_call_count += count_tool_calls(msg);
    }

    for event in &events {
        let Some(ev) = event.as_dict() else { continue };
        if string_field(ev, "visibility").as_deref() == Some("visible") {
            stats.visible_event_count += 1;
        }
    }

    stats
}

fn events_list(transcript: &VmValue) -> Vec<VmValue> {
    transcript
        .as_dict()
        .map(|dict| list_field(dict, "events"))
        .unwrap_or_default()
}

fn list_field(dict: &BTreeMap<String, VmValue>, key: &str) -> Vec<VmValue> {
    match dict.get(key) {
        Some(VmValue::List(list)) => (**list).clone(),
        _ => Vec::new(),
    }
}

fn string_field(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(VmValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn count_tool_calls(msg: &BTreeMap<String, VmValue>) -> i64 {
    match msg.get("tool_calls") {
        Some(VmValue::List(list)) => list.len() as i64,
        _ => 0,
    }
}

fn stats_to_vm(stats: &TranscriptStats) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "message_count".to_string(),
        VmValue::Int(stats.message_count),
    );
    dict.insert(
        "user_facing_message_count".to_string(),
        VmValue::Int(stats.user_facing_message_count),
    );
    dict.insert(
        "tool_result_message_count".to_string(),
        VmValue::Int(stats.tool_result_message_count),
    );
    dict.insert(
        "tool_call_count".to_string(),
        VmValue::Int(stats.tool_call_count),
    );
    dict.insert(
        "assistant_message_count".to_string(),
        VmValue::Int(stats.assistant_message_count),
    );
    dict.insert(
        "user_message_count".to_string(),
        VmValue::Int(stats.user_message_count),
    );
    dict.insert(
        "visible_event_count".to_string(),
        VmValue::Int(stats.visible_event_count),
    );
    VmValue::Dict(Rc::new(dict))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdlib::json_to_vm_value;

    fn transcript_from_json(value: serde_json::Value) -> VmValue {
        json_to_vm_value(&value)
    }

    #[test]
    fn stats_nil_transcript_returns_zeroes() {
        let stats = compute_transcript_stats(&VmValue::Nil);
        assert_eq!(stats, TranscriptStats::default());
    }

    #[test]
    fn stats_counts_roles_across_messages() {
        let transcript = transcript_from_json(serde_json::json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hey"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "a"}, {"id": "b"}]},
                {"role": "tool", "content": "ok", "tool_use_id": "a"},
                {"role": "tool_result", "content": "ok", "tool_use_id": "b"},
                {"role": "system", "content": "ignore me"},
            ],
        }));
        let stats = compute_transcript_stats(&transcript);
        assert_eq!(stats.message_count, 6);
        assert_eq!(stats.user_message_count, 1);
        assert_eq!(stats.assistant_message_count, 2);
        assert_eq!(stats.user_facing_message_count, 3);
        assert_eq!(stats.tool_result_message_count, 2);
        assert_eq!(stats.tool_call_count, 2);
    }

    #[test]
    fn stats_counts_visible_events() {
        let transcript = transcript_from_json(serde_json::json!({
            "events": [
                {"kind": "message", "visibility": "visible"},
                {"kind": "message", "visibility": "visible"},
                {"kind": "tool_result", "visibility": "hidden"},
            ],
        }));
        let stats = compute_transcript_stats(&transcript);
        assert_eq!(stats.visible_event_count, 2);
    }

    #[test]
    fn stats_tolerate_missing_keys() {
        let transcript = transcript_from_json(serde_json::json!({
            "messages": [
                {"role": "assistant"},
                {"content": "no role"},
            ],
        }));
        let stats = compute_transcript_stats(&transcript);
        assert_eq!(stats.message_count, 2);
        assert_eq!(stats.assistant_message_count, 1);
    }
}
