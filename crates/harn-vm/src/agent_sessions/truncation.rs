use super::*;
use crate::value::VmDictExt;

/// Truncate stored prompt history before acknowledging the host operation.
/// The run writer lease refuses a still-active prompt instead of racing its
/// journal. VM-only sessions retain the synchronous transcript operation.
pub async fn truncate_canonical(
    store: &dyn harn_session_store::SessionStore,
    store_dir: &Path,
    id: &str,
    keep_first: usize,
) -> Result<Option<SessionTruncateResult>, crate::value::VmError> {
    use crate::value::VmError;
    use harn_session_store::{SessionWriteLease, StoreError};

    if !exists(id) {
        return Ok(None);
    }
    let _writer = SessionWriteLease::try_acquire(store_dir, id)
        .map_err(|error| VmError::Runtime(format!("canonical session truncate: {error}")))?;
    match store.describe(id).await {
        Ok(meta) if !meta.has_canonical_journal() => {
            return truncate(id, keep_first).map_err(VmError::Runtime);
        }
        Ok(_) => {}
        Err(StoreError::NotFound(_)) => return truncate(id, keep_first).map_err(VmError::Runtime),
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "canonical session truncate: {error}"
            )))
        }
    }
    let events = store
        .read_all(id)
        .await
        .map_err(|error| VmError::Runtime(format!("canonical session truncate: {error}")))?;
    let hydrated = crate::agent_session_journal::hydrate_events(events);
    hydrated.restore(id).map_err(VmError::Runtime)?;
    let previous =
        snapshot(id).ok_or_else(|| VmError::Runtime(format!("unknown session '{id}'")))?;
    let outcome = truncate_outcome(id, keep_first).map_err(VmError::Runtime)?;
    if let Some(outcome) = &outcome {
        let result = &outcome.result;
        if result.removed_turn_count > 0 {
            let event = crate::agent_session_journal::messages_replaced_event(
                messages_json(id),
                None,
                hydrated
                    .source_event_ids
                    .into_iter()
                    .take(result.kept_turn_count)
                    .collect(),
            );
            if let Err(error) = store.append(id, event).await {
                store_transcript(id, previous).map_err(VmError::Runtime)?;
                return Err(VmError::Runtime(format!(
                    "canonical session truncate: {error}"
                )));
            }
        }
    }
    Ok(outcome.map(|outcome| report_truncated(id, outcome)))
}

/// Truncate the session transcript to the first `keep_first` messages.
pub fn truncate(id: &str, keep_first: usize) -> Result<Option<SessionTruncateResult>, String> {
    Ok(truncate_outcome(id, keep_first)?.map(|outcome| report_truncated(id, outcome)))
}

fn truncate_outcome(id: &str, keep_first: usize) -> Result<Option<SessionTruncateOutcome>, String> {
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(id) else {
            return Ok(None);
        };
        truncate_state(state, keep_first)
    })
}

fn report_truncated(id: &str, outcome: SessionTruncateOutcome) -> SessionTruncateResult {
    if outcome.result.removed_turn_count > 0 {
        crate::boundary::BoundaryFailure::new(
            crate::boundary::BoundaryId::SessionTranscript,
            crate::boundary::BoundaryFailureKind::Truncated,
            "agent session transcript was truncated to its leading messages",
        )
        .with_count(outcome.result.removed_turn_count)
        .with_dropped_bytes(outcome.dropped_bytes)
        .in_session(id)
        .report();
    }
    outcome.result
}

pub(super) struct SessionTruncateOutcome {
    result: SessionTruncateResult,
    dropped_bytes: usize,
}

pub(super) fn truncate_state(
    state: &mut SessionState,
    keep_first: usize,
) -> Result<Option<SessionTruncateOutcome>, String> {
    let dict = state
        .transcript
        .as_dict()
        .cloned()
        .unwrap_or_else(crate::value::DictMap::new);
    let messages: Vec<VmValue> = match dict.get("messages") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => Vec::new(),
    };
    let existing_events = match dict.get("events") {
        Some(VmValue::List(list)) => Some(list.iter().cloned().collect::<Vec<_>>()),
        _ => None,
    };
    let kept_turn_count = keep_first.min(messages.len());
    let removed_turn_count = messages.len().saturating_sub(kept_turn_count);
    let dropped_bytes = messages[kept_turn_count..]
        .iter()
        .map(|message| crate::llm::vm_value_to_json(message).to_string().len())
        .sum();
    let mut new_tip_turn_id = existing_events
        .as_ref()
        .map(|events| turn_event_id_for_count(events, kept_turn_count))
        .unwrap_or_else(|| {
            let events = crate::llm::helpers::transcript_events_from_messages(&messages);
            turn_event_id_for_count(&events, kept_turn_count)
        });

    if removed_turn_count > 0 {
        let retained: Vec<VmValue> = messages.into_iter().take(kept_turn_count).collect();
        let retained_events = match existing_events {
            Some(events) => {
                let keep_event_count = event_prefix_len_for_messages(&events, kept_turn_count);
                events.into_iter().take(keep_event_count).collect()
            }
            None => crate::llm::helpers::transcript_events_from_messages(&retained),
        };
        new_tip_turn_id = turn_event_id_for_count(&retained_events, kept_turn_count);
        let mut next = dict;
        next.put("events", VmValue::list(retained_events));
        next.put("messages", VmValue::list(retained));
        next.remove("summary");
        apply_transcript_with_budget(state, VmValue::dict(next), "truncate")?;
    }
    state.touch();
    Ok(Some(SessionTruncateOutcome {
        result: SessionTruncateResult {
            kept_turn_count,
            removed_turn_count,
            new_tip_turn_id,
        },
        dropped_bytes,
    }))
}

/// Keep only the last `keep_last` messages in a live session.
pub fn trim(id: &str, keep_last: usize) -> Result<Option<usize>, String> {
    let outcome = SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(id) else {
            return Ok::<_, String>(None);
        };
        let Some(dict) = state.transcript.as_dict().cloned() else {
            return Ok::<_, String>(None);
        };
        let messages: Vec<VmValue> = match dict.get("messages") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let start = messages.len().saturating_sub(keep_last);
        let dropped_bytes = messages[..start]
            .iter()
            .map(|message| crate::llm::vm_value_to_json(message).to_string().len())
            .sum();
        let retained: Vec<VmValue> = messages.into_iter().skip(start).collect();
        let kept = retained.len();
        let mut next = dict;
        next.put(
            "events",
            VmValue::list(crate::llm::helpers::transcript_events_from_messages(
                &retained,
            )),
        );
        next.put("messages", VmValue::list(retained));
        apply_transcript_with_budget(state, VmValue::dict(next), "trim")?;
        Ok(Some((kept, start, dropped_bytes)))
    })?;
    let Some((kept, dropped_count, dropped_bytes)) = outcome else {
        return Ok(None);
    };
    if dropped_count > 0 {
        crate::boundary::BoundaryFailure::new(
            crate::boundary::BoundaryId::SessionTranscript,
            crate::boundary::BoundaryFailureKind::Truncated,
            "agent session transcript was trimmed to its trailing messages",
        )
        .with_count(dropped_count)
        .with_dropped_bytes(dropped_bytes)
        .in_session(id)
        .report();
    }
    Ok(Some(kept))
}
