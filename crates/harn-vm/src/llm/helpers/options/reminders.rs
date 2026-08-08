use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderedReminder {
    SystemText(String),
}

impl RenderedReminder {
    fn rendered_role(&self) -> String {
        "user".to_string()
    }

    fn rendered_bytes(&self) -> usize {
        match self {
            Self::SystemText(text) => text.len(),
        }
    }
}

pub(super) fn reminder_directive_text(reminder: &SystemReminder) -> String {
    format!(
        "<directive authority=\"{}\">\n{}\n</directive>",
        reminder.authority.as_str(),
        escape_xml_text(&reminder.body)
    )
}

pub(crate) fn render_pending_reminders(
    _caps: &crate::llm::capabilities::Capabilities,
    reminders: &[SystemReminder],
) -> Vec<RenderedReminder> {
    reminders
        .iter()
        .map(|reminder| RenderedReminder::SystemText(reminder_directive_text(reminder)))
        .collect()
}

pub(super) fn rendered_reminder_lifecycle(
    session_id: Option<&str>,
    turn_number: i64,
    reminders: &[SystemReminder],
    rendered: &[RenderedReminder],
) -> Vec<crate::llm::api::ReminderLifecycleEmission> {
    reminders
        .iter()
        .zip(rendered.iter())
        .map(|(reminder, rendered)| {
            let rendered_role = rendered.rendered_role();
            crate::llm::api::ReminderLifecycleEmission {
                session_id: session_id.map(str::to_string),
                turn_number,
                reminder_id: reminder.id.clone(),
                tags: reminder.tags.clone(),
                body: reminder.body.clone(),
                dedupe_key: reminder.dedupe_key.clone(),
                source: reminder.source.as_str().to_string(),
                role_hint: reminder.role_hint.as_str().to_string(),
                authority: reminder.authority.as_str().to_string(),
                rendered_role,
                body_bytes: reminder.body.len(),
                rendered_bytes: rendered.rendered_bytes(),
                ttl_turns: reminder.ttl_turns,
                propagate: reminder.propagate.as_str().to_string(),
                originating_agent_id: reminder.originating_agent_id.clone(),
            }
        })
        .collect()
}

pub(super) fn emit_dropped_reminder_lifecycle(session_id: &str, reminder_id: String, reason: &str) {
    emit_reminder_lifecycle_event(
        REMINDER_DROPPED_EVENT_KIND,
        serde_json::json!({
            "session_id": session_id,
            "reminder_id": reminder_id,
            "reason": reason,
        }),
    );
}

pub(crate) fn pending_reminders_from_session(session_id: Option<&str>) -> Vec<SystemReminder> {
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return Vec::new();
    };
    let Some(dict) = transcript.as_dict() else {
        return Vec::new();
    };
    let events = dict.get("events").or_else(|| dict.get("messages"));
    let Some(VmValue::List(items)) = events else {
        return Vec::new();
    };
    let mut reminders = Vec::new();
    let mut invalid_count = 0;
    for event in items.iter() {
        if let Some(reminder) = reminder_from_event(event) {
            if reminder.body.trim().is_empty() {
                invalid_count += 1;
                emit_dropped_reminder_lifecycle(session_id, reminder.id, "invalid");
                continue;
            }
            reminders.push(reminder);
            continue;
        }
        let Some(dict) = event.as_dict() else {
            continue;
        };
        if dict.get("kind").map(VmValue::display).as_deref() != Some(SYSTEM_REMINDER_EVENT_KIND) {
            continue;
        }
        invalid_count += 1;
        let reminder_id = dict
            .get("reminder")
            .and_then(VmValue::as_dict)
            .and_then(|reminder| reminder.get("id"))
            .map(VmValue::display)
            .filter(|id| !id.is_empty())
            .or_else(|| {
                dict.get("id")
                    .map(VmValue::display)
                    .filter(|id| !id.is_empty())
            })
            .unwrap_or_else(|| "invalid-reminder".to_string());
        emit_dropped_reminder_lifecycle(session_id, reminder_id, "invalid");
    }
    if invalid_count > 0 {
        crate::agent_sessions::prune_invalid_reminder_events(session_id);
    }
    dedupe_and_order_directives(reminders)
}

/// Enforce the directive envelope's one deduplication and precedence policy.
/// Authority wins before recency, first for an explicit producer key and then
/// for normalized model-visible content. The retained directives are emitted
/// in contract > corrective > advisory order, with transcript order preserved
/// inside a tier.
fn dedupe_and_order_directives(reminders: Vec<SystemReminder>) -> Vec<SystemReminder> {
    fn candidate_wins(
        candidate: &(usize, SystemReminder),
        current: &(usize, SystemReminder),
    ) -> bool {
        candidate.1.authority.priority() > current.1.authority.priority()
            || (candidate.1.authority.priority() == current.1.authority.priority()
                && candidate.0 > current.0)
    }

    let indexed: Vec<(usize, SystemReminder)> = reminders.into_iter().enumerate().collect();
    let mut winner_for_key = std::collections::HashMap::<String, (usize, SystemReminder)>::new();
    for candidate in &indexed {
        let Some(key) = candidate.1.dedupe_key.as_ref() else {
            continue;
        };
        match winner_for_key.get(key) {
            Some(current) if !candidate_wins(candidate, current) => {}
            _ => {
                winner_for_key.insert(key.clone(), candidate.clone());
            }
        }
    }
    let keyed: Vec<(usize, SystemReminder)> = indexed
        .into_iter()
        .filter(|candidate| match candidate.1.dedupe_key.as_ref() {
            Some(key) => winner_for_key
                .get(key)
                .is_some_and(|winner| winner.0 == candidate.0),
            None => true,
        })
        .collect();

    let mut winner_for_body = std::collections::HashMap::<String, (usize, SystemReminder)>::new();
    for candidate in &keyed {
        let normalized = candidate
            .1
            .body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        match winner_for_body.get(&normalized) {
            Some(current) if !candidate_wins(candidate, current) => {}
            _ => {
                winner_for_body.insert(normalized, candidate.clone());
            }
        }
    }
    let mut retained: Vec<(usize, SystemReminder)> = keyed
        .into_iter()
        .filter(|candidate| {
            let normalized = candidate
                .1
                .body
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            winner_for_body
                .get(&normalized)
                .is_some_and(|winner| winner.0 == candidate.0)
        })
        .collect();
    retained.sort_by_key(|(index, reminder)| {
        (std::cmp::Reverse(reminder.authority.priority()), *index)
    });
    retained.into_iter().map(|(_, reminder)| reminder).collect()
}

/// Append `text` to a message's `content`, preserving whatever shape the
/// content already uses (string or array of content blocks).
pub(super) fn append_text_to_message_content(content: &mut serde_json::Value, text: &str) {
    if let serde_json::Value::Array(existing) = content {
        existing.push(serde_json::json!({"type": "text", "text": text}));
        return;
    }
    if let serde_json::Value::String(existing) = content {
        *existing = format!("{existing}\n\n{text}");
        return;
    }
    if content.is_null() {
        *content = serde_json::Value::String(text.to_string());
        return;
    }
    *content = serde_json::Value::Array(vec![
        std::mem::take(content),
        serde_json::json!({"type": "text", "text": text}),
    ]);
}

/// Fold the coalesced `SystemText` reminder block into the trailing message
/// when that message is already a `user` turn, mirroring
/// Returns `false` when the last message is absent or not a `user` turn, so the caller can
/// instead append a fresh trailing `user` message. Appending strictly after
/// the final message also guarantees we never split a tool_call/tool_result
/// pair.
pub(super) fn try_append_user_reminder_text(
    messages: &mut [serde_json::Value],
    text: &str,
) -> bool {
    let Some(last) = messages.last_mut() else {
        return false;
    };
    let Some(last_obj) = last.as_object_mut() else {
        return false;
    };
    if last_obj.get("role").and_then(|role| role.as_str()) != Some("user") {
        return false;
    }
    let content = last_obj
        .entry("content".to_string())
        .or_insert(serde_json::Value::Null);
    append_text_to_message_content(content, text);
    true
}

pub(crate) fn apply_rendered_reminder_messages(
    messages: Vec<serde_json::Value>,
    rendered: &[RenderedReminder],
) -> Vec<serde_json::Value> {
    let mut messages = messages;
    let mut system_text_blocks: Vec<&str> = Vec::new();
    for reminder in rendered {
        match reminder {
            RenderedReminder::SystemText(text) => system_text_blocks.push(text),
        }
    }
    if !system_text_blocks.is_empty() {
        let coalesced = format!(
            "<context-directives>\nFollow these active directives. Contract directives override corrective directives; corrective directives override advisory directives.\n{}\n</context-directives>",
            system_text_blocks.join("\n")
        );
        if !try_append_user_reminder_text(&mut messages, &coalesced) {
            messages.push(serde_json::json!({"role": "user", "content": coalesced}));
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::helpers::{DirectiveAuthority, ReminderSource};

    fn reminder(body: &str, dedupe_key: Option<&str>) -> SystemReminder {
        let mut reminder = SystemReminder::new(body, ReminderSource::StdlibProvider, 0);
        reminder.dedupe_key = dedupe_key.map(str::to_string);
        reminder
    }

    /// harn#4731 defect #3: two reminders sharing a `dedupe_key` (e.g. a recap
    /// re-attached across a compaction) must render/emit at most once per
    /// iteration. Newest wins; first-seen order is preserved.
    #[test]
    fn dedup_prefers_authority_then_recency_and_orders_the_envelope() {
        let input = vec![
            reminder("recap v1", Some("post_compact_recap")),
            reminder("workspace anchor", Some("workspace_anchor")),
            reminder("recap v2", Some("post_compact_recap")),
            {
                let mut value = reminder("  workspace   anchor ", None);
                value.authority = DirectiveAuthority::Advisory;
                value
            },
            {
                let mut value = reminder("correct the loop", None);
                value.authority = DirectiveAuthority::Corrective;
                value
            },
        ];
        let out = dedupe_and_order_directives(input);
        let bodies: Vec<&str> = out.iter().map(|r| r.body.as_str()).collect();
        assert_eq!(
            bodies,
            vec!["workspace anchor", "recap v2", "correct the loop"]
        );
    }

    #[test]
    fn higher_authority_wins_even_when_the_duplicate_is_older() {
        let mut contract = reminder("contract", Some("same"));
        contract.authority = DirectiveAuthority::Contract;
        let mut corrective = reminder("corrective", Some("same"));
        corrective.authority = DirectiveAuthority::Corrective;

        let out = dedupe_and_order_directives(vec![contract, corrective]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "contract");
    }
}
