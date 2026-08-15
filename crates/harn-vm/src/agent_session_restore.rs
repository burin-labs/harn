//! Restore a session's replay stream from the canonical session store.
//!
//! `session/list` answers what exists by reading the canonical store
//! ([`crate::session_timeline::list_persisted_sessions`]). Restorability has to
//! answer from the same place, or a client can be handed an id it is then told
//! is unknown. The observability event log
//! ([`crate::orchestration::load_agent_session_replay_events`]) is a
//! best-effort telemetry sink — its per-session sink is registered only while a
//! prompt is running, is cleared between turns, and silently no-ops when no log
//! is installed on the emitting thread — so an empty replay there means "not
//! observed", never "does not exist".
//!
//! This module projects durable transcript rows into the same
//! [`AgentSessionReplayEvent`] stream the live path replays, so one restore
//! path serves both and the transcript a client gets back does not depend on
//! which sink happened to be live when the session ran.

use std::path::Path;

use crate::agent_events::{AgentEvent, ToolCallStatus, ToolMutationStatus};
use crate::orchestration::AgentSessionReplayEvent;
use crate::value::VmError;
use harn_session_store::{ReadRange, SessionEventKind, SessionStore, StoreError, StoredEvent};

/// One page of stored events per round trip. The store caps reads at its own
/// `MAX_READ_BATCH`; this keeps the loop's memory bounded either way.
const RESTORE_PAGE: usize = 512;

/// Read `session_id`'s durable transcript out of `project_root`'s canonical
/// store and project it into replayable agent events.
///
/// Returns `Ok(None)` only when the store genuinely does not know the session —
/// no store for this project, or no such row. That is the one condition under
/// which a caller may report an unknown session. `Ok(Some(events))` with an
/// empty vector is a real session that simply has no transcript yet, which is
/// still restorable.
pub async fn load_canonical_session_replay_events(
    project_root: &Path,
    session_id: &str,
) -> Result<Option<Vec<AgentSessionReplayEvent>>, VmError> {
    let Some(store) = crate::stdlib::session_store::open_existing_canonical_store(project_root)?
    else {
        return Ok(None);
    };
    load_canonical_session_replay_events_from_store(&store, session_id).await
}

/// Store-injected form of [`load_canonical_session_replay_events`], so tests
/// and non-SQLite hosts can exercise the projection without a project layout.
pub async fn load_canonical_session_replay_events_from_store(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<Option<Vec<AgentSessionReplayEvent>>, VmError> {
    match store.describe(session_id).await {
        Ok(_) => {}
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "canonical session store describe {session_id}: {error}"
            )))
        }
    }

    let mut events = Vec::new();
    let mut from = None;
    loop {
        let page = store
            .read(
                session_id,
                ReadRange {
                    from_event_id: from,
                    limit: Some(RESTORE_PAGE),
                    ..ReadRange::default()
                },
            )
            .await
            .map_err(|error| {
                VmError::Runtime(format!(
                    "canonical session store read {session_id}: {error}"
                ))
            })?;
        for stored in page.events {
            if let Some(event) = replay_event_from_stored(session_id, &stored) {
                events.push(event);
            }
        }
        match page.next_cursor {
            Some(cursor) => from = Some(cursor),
            None => break,
        }
    }
    Ok(Some(events))
}

/// Project one durable row into a replay event, or `None` when the row carries
/// no client-visible transcript (bookkeeping, usage checkpoints, audit rows).
fn replay_event_from_stored(
    session_id: &str,
    stored: &StoredEvent,
) -> Option<AgentSessionReplayEvent> {
    let transcript = stored.payload.get("transcript_event")?;
    if transcript
        .get("visibility")
        .and_then(serde_json::Value::as_str)
        == Some("internal")
    {
        return None;
    }
    let role = transcript
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let text = transcript
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let event = match (&stored.kind, role) {
        (SessionEventKind::Message, "user") => AgentEvent::UserMessage {
            session_id: session_id.to_string(),
            message_id: transcript
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&stored.record_hash)
                .to_string(),
            content: user_content_blocks(transcript, text),
        },
        (SessionEventKind::Message, _) if !text.is_empty() => AgentEvent::AgentMessageChunk {
            session_id: session_id.to_string(),
            content: text.to_string(),
        },
        (SessionEventKind::ToolCall, _) => AgentEvent::ToolCall {
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id(stored, transcript)?,
            tool_name: tool_name(transcript),
            kind: None,
            status: ToolCallStatus::Completed,
            raw_input: transcript
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            parsing: None,
            audit: None,
        },
        (SessionEventKind::ToolResult, _) => AgentEvent::ToolCallUpdate {
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id(stored, transcript)?,
            tool_name: tool_name(transcript),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::Value::String(text.to_string())),
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            mutation_status: ToolMutationStatus::Unknown,
            changed_paths: None,
            data: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        },
        _ => return None,
    };

    Some(AgentSessionReplayEvent {
        event_id: stored.event_id,
        kind: stored_kind_label(&stored.kind),
        occurred_at_ms: stored.ts_ms,
        event,
    })
}

/// A user message replays as ACP content blocks. Prefer the canonical `blocks`
/// the transcript already carries; fall back to a single text block so a row
/// written before blocks existed still restores its words.
fn user_content_blocks(transcript: &serde_json::Value, text: &str) -> Vec<serde_json::Value> {
    match transcript
        .get("blocks")
        .and_then(serde_json::Value::as_array)
    {
        Some(blocks) if !blocks.is_empty() => blocks.clone(),
        _ => vec![serde_json::json!({"type": "text", "text": text})],
    }
}

fn tool_call_id(stored: &StoredEvent, transcript: &serde_json::Value) -> Option<String> {
    transcript
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            stored
                .headers
                .get("tool_call_id")
                .filter(|value| !value.is_empty())
                .cloned()
        })
}

fn tool_name(transcript: &serde_json::Value) -> String {
    transcript
        .get("tool_name")
        .or_else(|| transcript.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool")
        .to_string()
}

fn stored_kind_label(kind: &SessionEventKind) -> String {
    match kind {
        SessionEventKind::Custom { custom_type } => custom_type.clone(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "message".to_string()),
    }
}

#[cfg(test)]
mod tests;
