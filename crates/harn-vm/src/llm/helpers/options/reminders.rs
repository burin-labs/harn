use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderedReminder {
    SystemText(String),
    Message(serde_json::Value),
}

impl RenderedReminder {
    fn rendered_role(&self) -> String {
        match self {
            Self::SystemText(_) => "system".to_string(),
            Self::Message(message) => message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("system")
                .to_string(),
        }
    }
}

pub(super) fn reminder_xml_text(reminder: &SystemReminder) -> String {
    format!(
        "<system-reminder>\n{}\n</system-reminder>",
        escape_xml_text(&reminder.body)
    )
}

pub(super) fn reminder_plain_text(reminder: &SystemReminder) -> String {
    format!("System reminder:\n{}", reminder.body)
}

pub(super) fn reminder_system_text(
    caps: &crate::llm::capabilities::Capabilities,
    reminder: &SystemReminder,
) -> String {
    if caps.prefers_xml_scaffolding {
        reminder_xml_text(reminder)
    } else {
        reminder_plain_text(reminder)
    }
}

pub(super) fn reminder_developer_message(reminder: &SystemReminder) -> RenderedReminder {
    RenderedReminder::Message(serde_json::json!({
        "role": "developer",
        "content": reminder_plain_text(reminder),
    }))
}

pub(super) fn reminder_user_block_message(
    caps: &crate::llm::capabilities::Capabilities,
    reminder: &SystemReminder,
    cache_control: bool,
) -> RenderedReminder {
    let mut block = serde_json::json!({
        "type": "text",
        "text": reminder_xml_text(reminder),
    });
    if cache_control && caps.prompt_caching {
        block["cache_control"] = serde_json::json!({"type": "ephemeral"});
    }
    RenderedReminder::Message(serde_json::json!({
        "role": "user",
        "content": [block],
    }))
}

pub(crate) fn render_pending_reminders(
    caps: &crate::llm::capabilities::Capabilities,
    reminders: &[SystemReminder],
) -> Vec<RenderedReminder> {
    reminders
        .iter()
        .map(|reminder| {
            if caps.prefers_role_developer {
                return reminder_developer_message(reminder);
            }
            if caps.message_wire_format.is_anthropic() {
                return match reminder.role_hint {
                    ReminderRoleHint::UserBlock => {
                        reminder_user_block_message(caps, reminder, false)
                    }
                    ReminderRoleHint::EphemeralCache => {
                        reminder_user_block_message(caps, reminder, true)
                    }
                    ReminderRoleHint::System | ReminderRoleHint::Developer => {
                        RenderedReminder::SystemText(reminder_system_text(caps, reminder))
                    }
                };
            }
            RenderedReminder::SystemText(reminder_system_text(caps, reminder))
        })
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
                rendered_role,
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

pub(super) fn pending_reminders_from_session(session_id: Option<&str>) -> Vec<SystemReminder> {
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
    reminders
}

pub(super) fn prepend_content_blocks(
    content: &mut serde_json::Value,
    mut blocks: Vec<serde_json::Value>,
) {
    if let serde_json::Value::Array(existing) = content {
        blocks.append(existing);
        *existing = blocks;
        return;
    }
    if let serde_json::Value::String(text) = content {
        blocks.push(serde_json::json!({"type": "text", "text": text.clone()}));
        *content = serde_json::Value::Array(blocks);
        return;
    }
    if content.is_null() {
        *content = serde_json::Value::Array(blocks);
        return;
    }
    blocks.push(std::mem::take(content));
    *content = serde_json::Value::Array(blocks);
}

pub(super) fn try_prepend_user_reminder(
    messages: &mut [serde_json::Value],
    reminder: &serde_json::Value,
) -> bool {
    if reminder.get("role").and_then(|role| role.as_str()) != Some("user") {
        return false;
    }
    let Some(blocks) = reminder
        .get("content")
        .and_then(|content| content.as_array())
        .cloned()
    else {
        return false;
    };
    let Some(first) = messages.first_mut() else {
        return false;
    };
    let Some(first_obj) = first.as_object_mut() else {
        return false;
    };
    if first_obj.get("role").and_then(|role| role.as_str()) != Some("user") {
        return false;
    }
    let content = first_obj
        .entry("content".to_string())
        .or_insert(serde_json::Value::Null);
    prepend_content_blocks(content, blocks);
    true
}

/// Append `text` to a message's `content`, preserving whatever shape the
/// content already uses (string or array of content blocks). Mirrors the tail
/// counterpart of [`prepend_content_blocks`].
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
/// [`try_prepend_user_reminder`] but appending to the tail. Returns `false`
/// when the last message is absent or not a `user` turn, so the caller can
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

pub(super) fn apply_rendered_reminder_messages(
    messages: Vec<serde_json::Value>,
    rendered: &[RenderedReminder],
) -> Vec<serde_json::Value> {
    let mut messages = messages;
    let mut prefix = Vec::new();
    let mut system_text_blocks: Vec<&str> = Vec::new();
    for reminder in rendered {
        match reminder {
            RenderedReminder::Message(message) => {
                if !try_prepend_user_reminder(&mut messages, message) {
                    prefix.push(message.clone());
                }
            }
            // `SystemText` reminders used to be folded into the `system`
            // string. They are now coalesced into a single trailing `user`
            // message so the `system` string stays byte-identical across turns
            // (keeping the llama.cpp / non-Anthropic KV prefix cache warm). The
            // text is unchanged — `reminder_system_text` already wrapped each
            // body in its `<system-reminder>` scaffolding.
            RenderedReminder::SystemText(text) => system_text_blocks.push(text),
        }
    }
    prefix.extend(messages);
    if !system_text_blocks.is_empty() {
        let coalesced = system_text_blocks.join("\n\n");
        if !try_append_user_reminder_text(&mut prefix, &coalesced) {
            prefix.push(serde_json::json!({"role": "user", "content": coalesced}));
        }
    }
    prefix
}
