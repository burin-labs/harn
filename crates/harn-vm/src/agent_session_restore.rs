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
use crate::agent_sessions::event_facts as facts;
use crate::orchestration::AgentSessionReplayEvent;
use crate::value::VmError;
use harn_session_store::{
    chain_root_fold, chain_root_init, EventId, ReadRange, SessionEventKind, SessionMeta,
    SessionStore, StoreError, StoredEvent,
};

/// One page of stored events per round trip. The store caps reads at its own
/// `MAX_READ_BATCH`; this keeps the loop's memory bounded either way.
const RESTORE_PAGE: usize = 512;

/// A replay and its durable checkpoint describe the same captured prefix.
/// The checkpoint includes bookkeeping rows that have no visible projection.
pub struct CanonicalSessionReplay {
    pub events: Vec<AgentSessionReplayEvent>,
    pub last_event_id: Option<EventId>,
}

/// Read `session_id`'s durable transcript out of `project_root`'s canonical
/// store and project it into replayable agent events.
///
/// Returns `Ok(None)` only when the store genuinely does not know the session —
/// no store for this project, or no such row. That is the one condition under
/// which a caller may report an unknown session. A replay with an
/// empty event vector is a real session that simply has no transcript yet, which is
/// still restorable.
pub async fn load_canonical_session_replay(
    project_root: &Path,
    session_id: &str,
) -> Result<Option<CanonicalSessionReplay>, VmError> {
    let Some(store) = crate::stdlib::session_store::open_existing_canonical_store(project_root)?
    else {
        return Ok(None);
    };
    load_canonical_session_replay_from_store(&store, session_id).await
}

/// Store-injected form of [`load_canonical_session_replay`]. Capture the
/// durable checkpoint before reading and validate that exact prefix.
pub async fn load_canonical_session_replay_from_store(
    store: &dyn SessionStore,
    session_id: &str,
) -> Result<Option<CanonicalSessionReplay>, VmError> {
    let checkpoint = match store.describe(session_id).await {
        Ok(checkpoint) => checkpoint,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "canonical session store describe {session_id}: {error}"
            )))
        }
    };
    read_canonical_session_prefix(store, session_id, checkpoint)
        .await
        .map(Some)
}

async fn read_canonical_session_prefix(
    store: &dyn SessionStore,
    session_id: &str,
    checkpoint: SessionMeta,
) -> Result<CanonicalSessionReplay, VmError> {
    let invalid_prefix = || {
        VmError::Runtime(format!(
            "canonical session store prefix changed while restoring {session_id}"
        ))
    };
    if checkpoint.id != session_id {
        return Err(invalid_prefix());
    }
    // An absent upper bound means an unbounded read, not an empty session.
    if checkpoint.last_event_id.is_none() {
        if checkpoint.event_count != 0 {
            return Err(invalid_prefix());
        }
        return Ok(CanonicalSessionReplay {
            events: Vec::new(),
            last_event_id: None,
        });
    }

    let mut events = Vec::new();
    let mut event_count = 0;
    let mut last_event_id = None;
    let mut chain_root = chain_root_init();
    let mut from = None;
    loop {
        let page = store
            .read(
                session_id,
                ReadRange {
                    from_event_id: from,
                    to_event_id: checkpoint.last_event_id,
                    limit: Some(RESTORE_PAGE),
                },
            )
            .await
            .map_err(|error| {
                VmError::Runtime(format!(
                    "canonical session store read {session_id}: {error}"
                ))
            })?;
        for stored in page.events {
            if stored.session_id != session_id
                || Some(stored.event_id) > checkpoint.last_event_id
                || last_event_id.is_some_and(|previous| previous >= stored.event_id)
            {
                return Err(invalid_prefix());
            }
            event_count += 1;
            if event_count > checkpoint.event_count {
                return Err(invalid_prefix());
            }
            last_event_id = Some(stored.event_id);
            chain_root = chain_root_fold(&chain_root, stored.source_record_hash());
            // Projection remains private until the complete prefix is validated.
            if let Some(event) = replay_event_from_stored(session_id, &stored) {
                events.push(event);
            }
        }
        // A full final page may advertise tip + 1 as its next cursor. The
        // captured bound is already drained; do not reject or read beyond it.
        if last_event_id == checkpoint.last_event_id {
            break;
        }
        match page.next_cursor {
            Some(cursor) => {
                if from.is_some_and(|previous| cursor <= previous)
                    || Some(cursor) > checkpoint.last_event_id
                    || last_event_id.is_none_or(|event_id| cursor <= event_id)
                {
                    return Err(invalid_prefix());
                }
                from = Some(cursor);
            }
            None => break,
        }
    }
    if event_count != checkpoint.event_count
        || last_event_id != checkpoint.last_event_id
        || checkpoint.chain_root_hash.as_deref() != Some(chain_root.as_str())
    {
        return Err(invalid_prefix());
    }
    Ok(CanonicalSessionReplay {
        events: close_unanswered_tool_calls(session_id, events),
        last_event_id: checkpoint.last_event_id,
    })
}

/// What a restored call with no result reports. The store keeps a call from
/// the moment it starts, so a call whose process died mid-run has no result.
const UNANSWERED_TOOL_CALL_ERROR: &str = "No result: the session ended before this call finished.";

/// Close every replayed tool call that has no result with a failed update,
/// placed right after the call so a client applies it while that call is
/// still its current one.
///
/// A canonical restore reads a session no process is running, so a call
/// without a result will never get one. Replaying it as `completed` showed
/// an interrupted call as finished (harn#9061).
fn close_unanswered_tool_calls(
    session_id: &str,
    events: Vec<AgentSessionReplayEvent>,
) -> Vec<AgentSessionReplayEvent> {
    let answered: std::collections::HashSet<String> = events
        .iter()
        .filter_map(|replayed| match &replayed.event {
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                status: ToolCallStatus::Completed | ToolCallStatus::Failed,
                ..
            } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let mut last_call: std::collections::HashMap<String, (usize, String)> =
        std::collections::HashMap::new();
    for (index, replayed) in events.iter().enumerate() {
        if let AgentEvent::ToolCall {
            tool_call_id,
            tool_name,
            ..
        } = &replayed.event
        {
            if answered.contains(tool_call_id) {
                continue;
            }
            let name = last_call
                .get(tool_call_id)
                .map(|(_, name)| name.clone())
                .filter(|name| name != "tool")
                .unwrap_or_else(|| tool_name.clone());
            last_call.insert(tool_call_id.clone(), (index, name));
        }
    }
    let mut closed = Vec::with_capacity(events.len() + last_call.len());
    for (index, replayed) in events.into_iter().enumerate() {
        let close = match &replayed.event {
            AgentEvent::ToolCall { tool_call_id, .. } => last_call
                .get(tool_call_id)
                .filter(|(last, _)| *last == index)
                .map(|(_, name)| (tool_call_id.clone(), name.clone())),
            _ => None,
        };
        let (event_id, occurred_at_ms) = (replayed.event_id, replayed.occurred_at_ms);
        closed.push(replayed);
        if let Some((tool_call_id, tool_name)) = close {
            closed.push(AgentSessionReplayEvent {
                event_id,
                kind: "tool_result".to_string(),
                occurred_at_ms,
                execution_id: None,
                event: AgentEvent::ToolCallUpdate {
                    session_id: session_id.to_string(),
                    tool_call_id,
                    tool_name,
                    status: ToolCallStatus::Failed,
                    raw_output: Some(serde_json::Value::String(
                        UNANSWERED_TOOL_CALL_ERROR.to_string(),
                    )),
                    error: Some(UNANSWERED_TOOL_CALL_ERROR.to_string()),
                    duration_ms: None,
                    execution_duration_ms: None,
                    error_category: None,
                    mutation_status: ToolMutationStatus::Unknown,
                    changed_paths: None,
                    data: None,
                    health: None,
                    executor: None,
                    parsing: None,
                    raw_input: None,
                    raw_input_partial: None,
                    audit: None,
                },
            });
        }
    }
    closed
}

/// Project one durable row into a replay event, or `None` when the row carries
/// no client-visible transcript (bookkeeping, usage checkpoints, audit rows).
fn replay_event_from_stored(
    session_id: &str,
    stored: &StoredEvent,
) -> Option<AgentSessionReplayEvent> {
    let transcript = stored.payload.get("transcript_event")?;
    let kind = transcript.get("kind").and_then(serde_json::Value::as_str);
    if matches!(
        kind,
        Some("turn_phase_changed" | "agent_run_terminal" | "progress_reported")
    ) {
        let metadata = transcript.get("metadata")?;
        let event = if kind == Some("agent_run_terminal") {
            AgentEvent::TurnPhaseChanged {
                session_id: session_id.to_string(),
                phase: crate::agent_events::AgentTurnPhase::from_terminal_record(metadata)?,
            }
        } else {
            AgentEvent::from_host_payload(session_id, kind?, metadata).ok()??
        };
        // A standalone phase row can precede a failed terminal write. Only the
        // committed run record owns finality; older records lack its reply.
        if kind == Some("turn_phase_changed")
            && matches!(
                event,
                AgentEvent::TurnPhaseChanged {
                    phase: crate::agent_events::AgentTurnPhase::Terminal { .. },
                    ..
                }
            )
        {
            return None;
        }
        return Some(AgentSessionReplayEvent {
            event_id: stored.event_id,
            kind: stored_kind_label(&stored.kind),
            occurred_at_ms: stored.ts_ms,
            execution_id: None,
            event,
        });
    }
    // Internal visibility hides bookkeeping and prose the model wrote for
    // itself. It does not hide tool rows: the journal writes every tool call
    // and result as internal, because its text is not conversation, yet a
    // client renders each as its own entry. Filtering them here restored a
    // session with every tool call missing (harn#8920).
    if matches!(stored.kind, SessionEventKind::Message)
        && transcript
            .get("visibility")
            .and_then(serde_json::Value::as_str)
            == Some("internal")
    {
        return None;
    }
    let raw_message = stored.payload.get("raw_message");
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
        (SessionEventKind::ToolCall, _) => {
            let tool_call_id = tool_call_id(stored, transcript)?;
            let provider_call = provider_tool_call(raw_message, &tool_call_id);
            AgentEvent::ToolCall {
                session_id: session_id.to_string(),
                tool_name: tool_name(transcript, raw_message, provider_call),
                tool_call_id,
                kind: None,
                status: ToolCallStatus::Completed,
                raw_input: transcript
                    .get("input")
                    .or_else(|| transcript.pointer("/metadata/raw_input"))
                    .or_else(|| provider_call.and_then(|call| call.get("arguments")))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                parsing: None,
                audit: None,
                intent: None,
            }
        }
        (SessionEventKind::ToolResult, _) => {
            let failed = facts::bool_at_any(&stored.payload, &facts::TOOL_IS_ERROR_ANY);
            let data = stored
                .payload
                .pointer(facts::TOOL_RESULT_DATA)
                .filter(|data| data.is_object());
            AgentEvent::ToolCallUpdate {
                session_id: session_id.to_string(),
                tool_call_id: tool_call_id(stored, transcript)?,
                tool_name: tool_name(transcript, raw_message, None),
                status: if failed {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                },
                raw_output: Some(serde_json::Value::String(text.to_string())),
                error: failed.then(|| text.to_string()),
                duration_ms: None,
                execution_duration_ms: None,
                error_category: None,
                mutation_status: mutation_status(data),
                changed_paths: changed_paths(data),
                data: data.cloned(),
                health: None,
                executor: None,
                parsing: None,
                raw_input: None,
                raw_input_partial: None,
                audit: None,
            }
        }
        _ => return None,
    };

    Some(AgentSessionReplayEvent {
        event_id: stored.event_id,
        kind: stored_kind_label(&stored.kind),
        occurred_at_ms: stored.ts_ms,
        execution_id: None,
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

/// The provider's own entry for `tool_call_id` in an assistant message's
/// `tool_calls`. An assistant turn's tool-call row carries the name only there.
fn provider_tool_call<'a>(
    raw_message: Option<&'a serde_json::Value>,
    tool_call_id: &str,
) -> Option<&'a serde_json::Value> {
    raw_message?
        .get("tool_calls")?
        .as_array()?
        .iter()
        .find(|call| call.get("id").and_then(serde_json::Value::as_str) == Some(tool_call_id))
}

/// Read the tool name from the transcript event, then from its `metadata`,
/// the same order the journal reads a tool call's identity when it writes the
/// row. Tool lifecycle events carry the name only under `metadata`; a tool
/// result carries it only on the provider message stored beside it; an
/// assistant turn's call carries it only in that message's `tool_calls`.
/// Without the last, a call cut off before its result replays as "tool"
/// (harn#9061).
fn tool_name(
    transcript: &serde_json::Value,
    raw_message: Option<&serde_json::Value>,
    provider_call: Option<&serde_json::Value>,
) -> String {
    [
        Some(transcript),
        transcript.get("metadata"),
        raw_message,
        provider_call,
        provider_call.and_then(|call| call.get("function")),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| {
        value
            .get("tool_name")
            .or_else(|| value.get("name"))
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
    })
    .unwrap_or("tool")
    .to_string()
}

/// The mutation outcome the producer declared, which the live path projects
/// onto the same update. An undeclared or unrecognized value stays unknown.
fn mutation_status(data: Option<&serde_json::Value>) -> ToolMutationStatus {
    let declared = data
        .and_then(|data| data.get("mutation_status"))
        .and_then(serde_json::Value::as_str);
    ToolMutationStatus::ALL
        .into_iter()
        .find(|status| Some(status.as_str()) == declared)
        .unwrap_or(ToolMutationStatus::Unknown)
}

fn changed_paths(data: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let paths = data?.get("changed_paths")?.as_array()?;
    Some(
        paths
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_string)
            .collect(),
    )
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
