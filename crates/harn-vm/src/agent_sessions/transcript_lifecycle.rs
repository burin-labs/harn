use super::*;

pub fn messages_json(id: &str) -> Vec<serde_json::Value> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return Vec::new();
        };
        let Some(dict) = state.transcript.as_dict() else {
            return Vec::new();
        };
        match dict.get("messages") {
            Some(VmValue::List(list)) => list
                .iter()
                .map(crate::llm::helpers::vm_value_to_json)
                .collect(),
            _ => Vec::new(),
        }
    })
}

#[derive(Clone, Debug, Default)]
pub struct SessionPromptState {
    pub messages: Vec<serde_json::Value>,
    pub summary: Option<String>,
}

fn summary_message_json(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": summary,
    })
}

fn messages_begin_with_summary(messages: &[serde_json::Value], summary: &str) -> bool {
    messages.first().is_some_and(|message| {
        message.get("role").and_then(|value| value.as_str()) == Some("user")
            && message.get("content").and_then(|value| value.as_str()) == Some(summary)
    })
}

/// Prompt-surface resume state for a persisted session.
///
/// Returns the compacted/rehydratable message list plus the transcript's
/// summary field. When the transcript carries a summary field but its
/// message list does not already begin with the compacted summary
/// message, this helper prepends one so session re-entry preserves the
/// same prompt surface the previous loop was actually using.
pub fn prompt_state_json(id: &str) -> SessionPromptState {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return SessionPromptState::default();
        };
        let Some(dict) = state.transcript.as_dict() else {
            return SessionPromptState::default();
        };
        let mut messages = match dict.get("messages") {
            Some(VmValue::List(list)) => list
                .iter()
                .map(crate::llm::helpers::vm_value_to_json)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let summary = dict.get("summary").and_then(|value| match value {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        });
        if let Some(summary_text) = summary.as_deref() {
            if !messages_begin_with_summary(&messages, summary_text) {
                messages.insert(0, summary_message_json(summary_text));
            }
        }
        SessionPromptState { messages, summary }
    })
}

/// Overwrite the transcript for this session. Used by `agent_loop` on
/// exit to persist the synthesized transcript.
pub fn store_transcript(id: &str, transcript: VmValue) -> Result<(), String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_store_transcript: unknown session id '{id}'"
            ));
        };
        let transcript = transcript_with_session_metadata(transcript, state);
        let text_tool_call_seq = next_text_tool_call_seq_from_transcript(&transcript);
        apply_transcript_with_budget(state, transcript, "store_transcript")?;
        state.text_tool_call_seq = state.text_tool_call_seq.max(text_tool_call_seq);
        Ok(())
    })
}

fn checkpoint_summary(checkpoint: &SessionTurnCheckpoint) -> SessionCheckpointSummary {
    SessionCheckpointSummary {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        before_message_count: checkpoint.before_message_count,
        after_message_count: checkpoint.after_message_count,
        fs_snapshot_ids: checkpoint.fs_snapshot_ids.clone(),
    }
}

fn checkpoint_error_status(error: SessionCheckpointError) -> &'static str {
    match error {
        SessionCheckpointError::UnknownSession => "unknown_session",
        SessionCheckpointError::NoCheckpoint => "no_checkpoint",
        SessionCheckpointError::NoRedo => "no_redo",
    }
}

pub fn checkpoint_status_name(error: SessionCheckpointError) -> &'static str {
    checkpoint_error_status(error)
}

/// Clear redo checkpoints after host-side workspace mutations that are not part
/// of the redo flow. Returns whether any redo state was discarded.
pub fn invalidate_redo(id: &str) -> bool {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return false;
        };
        let had_redo = !state.redo_stack.is_empty();
        state.redo_stack.clear();
        state.touch();
        had_redo
    })
}

/// Record a completed prompt turn boundary.
///
/// `before_transcript` must be captured immediately before the user turn
/// starts. The current live transcript becomes the redo target, and optional
/// `fs_snapshot_ids` name host-owned filesystem snapshots captured during the
/// turn. Harn owns the transcript stack; hosts own concrete file restoration.
pub fn record_completed_turn_checkpoint(
    id: &str,
    before_transcript: VmValue,
    fs_snapshot_ids: Vec<String>,
) -> Result<Option<SessionCheckpointSummary>, SessionCheckpointError> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(SessionCheckpointError::UnknownSession);
        };
        let after_transcript = transcript_with_session_metadata(state.transcript.clone(), state);
        let before_message_count = transcript_message_count(&before_transcript);
        let after_message_count = transcript_message_count(&after_transcript);
        if crate::values_equal(&before_transcript, &after_transcript) && fs_snapshot_ids.is_empty()
        {
            return Ok(None);
        }
        let checkpoint = SessionTurnCheckpoint {
            checkpoint_id: format!("turn_{}", uuid::Uuid::now_v7().simple()),
            completed_at: crate::orchestration::now_unix_seconds_text(),
            before_message_count,
            after_message_count,
            before_transcript,
            after_transcript,
            fs_snapshot_ids,
        };
        state.redo_stack.clear();
        state.completed_turn_checkpoints.push(checkpoint.clone());
        state.touch();
        Ok(Some(checkpoint_summary(&checkpoint)))
    })
}

pub fn rollback_plan(id: &str) -> Result<SessionCheckpointSummary, SessionCheckpointError> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return Err(SessionCheckpointError::UnknownSession);
        };
        state
            .completed_turn_checkpoints
            .last()
            .map(checkpoint_summary)
            .ok_or(SessionCheckpointError::NoCheckpoint)
    })
}

pub fn redo_plan(id: &str) -> Result<SessionCheckpointSummary, SessionCheckpointError> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return Err(SessionCheckpointError::UnknownSession);
        };
        state
            .redo_stack
            .last()
            .map(|entry| {
                let mut summary = checkpoint_summary(&entry.checkpoint);
                summary.fs_snapshot_ids = entry.redo_fs_snapshot_ids.clone();
                summary
            })
            .ok_or(SessionCheckpointError::NoRedo)
    })
}

pub fn rollback_last_completed_turn(
    id: &str,
    redo_fs_snapshot_ids: Vec<String>,
) -> Result<SessionCheckpointOutcome, SessionCheckpointError> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(SessionCheckpointError::UnknownSession);
        };
        let Some(checkpoint) = state.completed_turn_checkpoints.pop() else {
            return Err(SessionCheckpointError::NoCheckpoint);
        };
        state.transcript = checkpoint.before_transcript.clone();
        state.redo_stack.push(SessionRedoEntry {
            checkpoint: checkpoint.clone(),
            redo_fs_snapshot_ids: redo_fs_snapshot_ids.clone(),
        });
        state.touch();
        Ok(SessionCheckpointOutcome {
            status: "rolled_back",
            checkpoint: checkpoint_summary(&checkpoint),
            redo_fs_snapshot_ids,
        })
    })
}

pub fn redo_last_rollback(id: &str) -> Result<SessionCheckpointOutcome, SessionCheckpointError> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(SessionCheckpointError::UnknownSession);
        };
        let Some(entry) = state.redo_stack.pop() else {
            return Err(SessionCheckpointError::NoRedo);
        };
        let checkpoint = entry.checkpoint;
        state.transcript = checkpoint.after_transcript.clone();
        state.completed_turn_checkpoints.push(checkpoint.clone());
        state.touch();
        Ok(SessionCheckpointOutcome {
            status: "redone",
            checkpoint: checkpoint_summary(&checkpoint),
            redo_fs_snapshot_ids: entry.redo_fs_snapshot_ids,
        })
    })
}

/// Remove malformed reminder events after their drop audit has been emitted.
/// Pending-reminder rendering scans the transcript on every LLM call; pruning
/// invalid entries makes the drop event one-shot instead of noisy per turn.
pub fn prune_invalid_reminder_events(id: &str) -> usize {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return 0;
        };
        let Some(dict) = state.transcript.as_dict().cloned() else {
            return 0;
        };
        let Some(VmValue::List(events)) = dict.get("events") else {
            return 0;
        };
        let mut pruned = 0_usize;
        let mut kept = Vec::with_capacity(events.len());
        for event in events.iter().cloned() {
            let is_reminder = event
                .as_dict()
                .and_then(|event| event.get("kind"))
                .map(VmValue::display)
                .as_deref()
                == Some(crate::llm::helpers::SYSTEM_REMINDER_EVENT_KIND);
            if !is_reminder {
                kept.push(event);
                continue;
            }
            let valid = crate::llm::helpers::reminder_from_event(&event)
                .is_some_and(|reminder| !reminder.body.trim().is_empty());
            if valid {
                kept.push(event);
            } else {
                pruned += 1;
            }
        }
        if pruned > 0 {
            let mut next = dict;
            next.insert(
                crate::value::intern_key("events"),
                VmValue::List(std::sync::Arc::new(kept)),
            );
            let _ = apply_transcript_with_budget(
                state,
                VmValue::dict(next),
                "prune_invalid_reminder_events",
            );
            state.touch();
        }
        pruned
    })
}

/// Apply the reminder TTL lifecycle that runs once per completed agent
/// turn. Reminders with `ttl_turns = 1` expire and are removed; larger
/// finite TTLs are decremented in place. Expiry audit events are emitted
/// to the active EventLog when one is installed.
pub fn apply_reminder_post_turn(id: &str, turn: i64) -> Result<serde_json::Value, String> {
    let report = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_apply_reminder_post_turn: unknown session id '{id}'"
            ));
        };
        let report = crate::llm::helpers::apply_reminder_post_turn(&state.transcript, turn);
        if report.decremented_count > 0 || !report.expired.is_empty() {
            if let Some(next) = report.transcript.clone() {
                apply_transcript_with_budget(state, next, "apply_reminder_post_turn")?;
            }
            state.touch();
        }
        Ok(report)
    })?;

    for reminder in &report.expired {
        let mut payload = crate::llm::helpers::reminder_lifecycle_payload(Some(id), reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::Value::String(id.to_string()),
            );
            obj.insert(
                "reason".to_string(),
                serde_json::Value::String("ttl".to_string()),
            );
            obj.insert(
                "ttl_turns_before".to_string(),
                serde_json::json!(&reminder.ttl_turns),
            );
            obj.insert("expired_at_turn".to_string(), serde_json::json!(turn));
        }
        crate::llm::helpers::emit_reminder_lifecycle_event(
            crate::llm::helpers::REMINDER_EXPIRED_EVENT_KIND,
            payload,
        );
    }

    Ok(serde_json::json!({
        "expired_count": report.expired.len(),
        "decremented_count": report.decremented_count,
        "remaining_count": report.remaining_count,
    }))
}

/// Inject a typed system reminder into the session transcript's event
/// stream. This mirrors `transcript.inject_reminder` for live sessions:
/// reminders with the same `dedupe_key` are replaced before the new
/// reminder event is appended.
pub fn inject_reminder(
    id: &str,
    reminder: crate::llm::helpers::SystemReminder,
) -> Result<ReminderInjectionReport, String> {
    let reminder_id = reminder.id.clone();
    let dedupe_key = reminder.dedupe_key.clone();
    let mut deduped_reminder_ids = Vec::new();
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_inject_reminder: unknown session id '{id}'"
            ));
        };
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(crate::value::DictMap::new);
        let mut events: Vec<VmValue> = match dict.get("events") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => dict
                .get("messages")
                .and_then(|value| match value {
                    VmValue::List(list) => Some(list.iter().cloned().collect::<Vec<_>>()),
                    _ => None,
                })
                .map(|messages| crate::llm::helpers::transcript_events_from_messages(&messages))
                .unwrap_or_default(),
        };
        if let Some(expected_key) = dedupe_key.as_deref() {
            events.retain(|event| {
                let Some(existing) = crate::llm::helpers::reminder_from_event(event) else {
                    return true;
                };
                if existing.dedupe_key.as_deref() == Some(expected_key) {
                    deduped_reminder_ids.push(existing.id);
                    false
                } else {
                    true
                }
            });
        }
        events.push(crate::llm::helpers::transcript_reminder_event(&reminder));
        let mut next = dict;
        next.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(events)),
        );
        apply_transcript_with_budget(state, VmValue::dict(next), "inject_reminder")?;
        state.touch();
        Ok(())
    })?;

    if !deduped_reminder_ids.is_empty() {
        let dropped_count = deduped_reminder_ids.len();
        crate::llm::helpers::emit_reminder_lifecycle_event(
            crate::llm::helpers::REMINDER_DEDUPED_EVENT_KIND,
            serde_json::json!({
                "session_id": id,
                "transcript_id": id,
                "reminder_id": &reminder_id,
                "replacing_id": &reminder_id,
                "replaced_id": deduped_reminder_ids.first(),
                "replaced_ids": &deduped_reminder_ids,
                "dedupe_key": &dedupe_key,
                "dropped_reminder_ids": &deduped_reminder_ids,
                "dropped_count": dropped_count,
            }),
        );
    }

    crate::llm::helpers::emit_reminder_lifecycle_event(
        crate::llm::helpers::REMINDER_INJECTED_EVENT_KIND,
        crate::llm::helpers::reminder_lifecycle_payload(Some(id), &reminder),
    );

    Ok(ReminderInjectionReport {
        reminder_id,
        deduped_count: deduped_reminder_ids.len(),
    })
}
