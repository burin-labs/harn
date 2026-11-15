//! Project repeated goal objectives from the final selected context.
//! Canonical reminder records and the existing envelope renderer own the text;
//! caller-supplied origins, anchors and byte ranges have no role here.

use super::reminders::{reminder_directive_text, DIRECTIVE_IDS_KEY};
use crate::llm::helpers::SystemReminder;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

fn person_text(message: &Value) -> Option<&str> {
    if message.get("role").and_then(Value::as_str) != Some("user")
        || message.get(DIRECTIVE_IDS_KEY).is_some()
        || harn_orchestration::tool_messages::is_tool_output_message(message)
    {
        return None;
    }
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text);
    }
    let mut texts = content.as_array()?.iter().filter_map(|block| {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| block.get("text").and_then(Value::as_str))
            .flatten()
    });
    let text = texts.next()?;
    texts.next().is_none().then_some(text)
}

fn directive_ids(message: &Value) -> Option<Vec<&str>> {
    message
        .get(DIRECTIVE_IDS_KEY)?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect()
}

pub(super) fn project_goal_context(
    messages: &mut [Value],
    session_id: Option<&str>,
    pending: &[SystemReminder],
) -> HashMap<String, usize> {
    let mut rendered_bytes = HashMap::new();
    let Some(session_id) = session_id else {
        return rendered_bytes;
    };
    if !pending.iter().any(|reminder| reminder.goal_pin.is_some()) {
        return rendered_bytes;
    }
    let needed = messages
        .iter()
        .filter_map(directive_ids)
        .flatten()
        .collect::<HashSet<_>>();
    if needed.is_empty() {
        return rendered_bytes;
    }
    let records = crate::agent_sessions::goal_envelope_reminders(session_id, &needed, pending);
    if !records.values().any(|reminder| reminder.goal_pin.is_some()) {
        return rendered_bytes;
    }
    let covered_objectives = {
        let texts = messages
            .iter()
            .filter_map(person_text)
            .collect::<HashSet<_>>();
        records
            .values()
            .filter_map(|reminder| reminder.goal_pin.as_ref())
            .filter(|pin| texts.contains(pin.spec.objective.as_str()))
            .map(|pin| pin.spec.objective.clone())
            .collect::<HashSet<_>>()
    };
    if covered_objectives.is_empty() {
        return rendered_bytes;
    }
    for message in messages {
        let Some(ids) = directive_ids(message) else {
            continue;
        };
        // Validate the whole immutable stored envelope before changing any
        // block. Current TTL state of unrelated siblings is irrelevant.
        if !crate::agent_sessions::canonical_directive_envelope_matches(session_id, message) {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_str) else {
            continue;
        };
        let mut replacements = Vec::new();
        for id in ids {
            let Some(reminder) = records.get(id) else {
                continue;
            };
            let Some(pin) = &reminder.goal_pin else {
                continue;
            };
            if !covered_objectives.contains(&pin.spec.objective)
                || reminder.body == pin.objective_covered_body
            {
                continue;
            }
            let full = reminder_directive_text(reminder);
            // Match a complete machine-rendered directive, not arbitrary goal
            // substrings. CDATA escaping prevents a quoted sibling body from
            // containing this complete closing block. Ambiguity fails closed.
            let mut matches = content.match_indices(full.as_str());
            let Some((start, _)) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                continue;
            }
            let end = start + full.len();
            if start > 0 && content.as_bytes()[start - 1] != b'\n'
                || end < content.len() && content.as_bytes()[end] != b'\n'
            {
                continue;
            }
            let mut covered = reminder.clone();
            covered.body = pin.objective_covered_body.clone();
            replacements.push((
                start,
                end,
                id.to_string(),
                reminder_directive_text(&covered),
            ));
        }
        replacements.sort_by_key(|replacement| replacement.0);
        if replacements.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            continue;
        }
        if replacements.is_empty() {
            continue;
        }
        let mut projected = String::with_capacity(content.len());
        let mut cursor = 0;
        for (start, end, id, text) in replacements {
            projected.push_str(&content[cursor..start]);
            projected.push_str(&text);
            rendered_bytes.insert(id, text.len());
            cursor = end;
        }
        projected.push_str(&content[cursor..]);
        message["content"] = Value::String(projected);
    }
    rendered_bytes
}

#[cfg(test)]
#[path = "goal_context_tests.rs"]
mod tests;
