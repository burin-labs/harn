//! Durable mutation journal for live agent transcripts.
//!
//! Transcript mutation remains synchronous and VM-local. This module records
//! the exact mutations, then flushes them through the canonical session-store
//! at existing async agent-loop boundaries. `AgentEvent` sinks stay strictly
//! observability-only: a filtered or failed sink can never alter durability.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use harn_session_store::{
    AppendEvent, EventIdentity, EventIdentityField, SessionEventKind, SessionStore,
};

use crate::stdlib::session_store;
use crate::value::{DictMap, VmError};

#[derive(Clone)]
struct JournalConfig {
    root: PathBuf,
    run_id: String,
    turn_id: String,
}

#[derive(Clone)]
enum TranscriptMutation {
    MessageAdded {
        transcript_event: serde_json::Value,
        raw_message: serde_json::Value,
    },
    AuditEventAdded {
        transcript_event: serde_json::Value,
    },
    MessagesReplaced {
        messages: Vec<serde_json::Value>,
        summary: Option<String>,
        source_event_ids: Vec<Option<String>>,
    },
    MessageRemoved {
        source_event_id: String,
        raw_message: serde_json::Value,
    },
}

struct JournalState {
    config: JournalConfig,
    pending: VecDeque<TranscriptMutation>,
}

thread_local! {
    static JOURNALS: RefCell<std::collections::BTreeMap<String, JournalState>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

#[derive(Default)]
pub(crate) struct HydratedTranscript {
    pub messages: Vec<serde_json::Value>,
    pub source_event_ids: Vec<Option<String>>,
}

/// Open the canonical session and install its per-agent-run journal before
/// lifecycle hooks can mutate the in-memory transcript.
pub(crate) async fn hydrate_and_configure(
    session_id: &str,
    options: &DictMap,
    run_id: String,
    turn_id: String,
) -> Result<HydratedTranscript, VmError> {
    let root = session_store::canonical_store_root(Some(options))?;
    let store = session_store::open_canonical_agent_session(&root, session_id).await?;
    let events = session_store::read_all_events(&store, session_id).await?;
    let hydrated = hydrate_events(events);
    let config = JournalConfig {
        root,
        run_id,
        turn_id,
    };
    JOURNALS.with(|journals| {
        let mut journals = journals.borrow_mut();
        if journals
            .get(session_id)
            .is_some_and(|journal| !journal.pending.is_empty())
        {
            return Err(VmError::Runtime(format!(
                "agent transcript journal for `{session_id}` has unflushed mutations"
            )));
        }
        journals.insert(
            session_id.to_string(),
            JournalState {
                config,
                pending: VecDeque::new(),
            },
        );
        Ok(())
    })?;
    Ok(hydrated)
}

pub(crate) fn enqueue_message(
    session_id: &str,
    transcript_event: serde_json::Value,
    raw_message: serde_json::Value,
) {
    enqueue(
        session_id,
        TranscriptMutation::MessageAdded {
            transcript_event,
            raw_message,
        },
    );
}

pub(crate) fn enqueue_audit_event(session_id: &str, transcript_event: serde_json::Value) {
    enqueue(
        session_id,
        TranscriptMutation::AuditEventAdded { transcript_event },
    );
}

pub(crate) fn enqueue_messages_replaced(
    session_id: &str,
    messages: Vec<serde_json::Value>,
    summary: Option<String>,
    source_event_ids: Vec<Option<String>>,
) {
    enqueue(
        session_id,
        TranscriptMutation::MessagesReplaced {
            messages,
            summary,
            source_event_ids,
        },
    );
}

pub(crate) fn enqueue_message_removed(
    session_id: &str,
    source_event_id: String,
    raw_message: serde_json::Value,
) {
    enqueue(
        session_id,
        TranscriptMutation::MessageRemoved {
            source_event_id,
            raw_message,
        },
    );
}

fn enqueue(session_id: &str, mutation: TranscriptMutation) {
    JOURNALS.with(|journals| {
        if let Some(journal) = journals.borrow_mut().get_mut(session_id) {
            journal.pending.push_back(mutation);
        }
    });
}

/// Persist queued mutations in chronological order. A mutation remains queued
/// until its individual append succeeds, so a later failure is observable and
/// retryable without a background writer or a best-effort drop.
pub(crate) async fn flush(session_id: &str) -> Result<(), VmError> {
    loop {
        let next = JOURNALS.with(|journals| {
            journals.borrow().get(session_id).and_then(|journal| {
                journal
                    .pending
                    .front()
                    .cloned()
                    .map(|mutation| (journal.config.clone(), mutation))
            })
        });
        let Some((config, mutation)) = next else {
            return Ok(());
        };

        let store = session_store::open_canonical_agent_session(&config.root, session_id).await?;
        let event = append_event_for_mutation(&config, mutation)?;
        store
            .append(session_id, event)
            .await
            .map_err(|error| VmError::Runtime(format!("agent transcript journal: {error}")))?;

        JOURNALS.with(|journals| {
            if let Some(journal) = journals.borrow_mut().get_mut(session_id) {
                journal.pending.pop_front();
            }
        });
    }
}

pub(crate) fn clear(session_id: &str) {
    JOURNALS.with(|journals| {
        journals.borrow_mut().remove(session_id);
    });
}

pub(crate) fn reset() {
    JOURNALS.with(|journals| journals.borrow_mut().clear());
}

fn append_event_for_mutation(
    config: &JournalConfig,
    mutation: TranscriptMutation,
) -> Result<AppendEvent, VmError> {
    match mutation {
        TranscriptMutation::MessageAdded {
            transcript_event,
            raw_message,
        } => {
            let kind = event_kind_for_transcript(&transcript_event, true);
            let actor = json_string(&transcript_event, "role");
            let mut event = AppendEvent::new(
                kind,
                serde_json::json!({
                    "transcript_event": transcript_event,
                    "raw_message": raw_message,
                }),
            );
            event.actor = actor;
            let source_event_id = source_event_id(&event.payload);
            let message_id = message_id(&event.payload);
            let tool_call_id = tool_call_id(&event.payload);
            if tool_call_id.is_some() && matches!(&event.kind, SessionEventKind::Message) {
                event.kind = SessionEventKind::ToolCall;
            }
            apply_identity(
                &mut event,
                config,
                source_event_id,
                message_id,
                tool_call_id,
            )?;
            Ok(event)
        }
        TranscriptMutation::AuditEventAdded { transcript_event } => {
            let kind = event_kind_for_transcript(&transcript_event, false);
            let actor = json_string(&transcript_event, "role");
            let mut event = AppendEvent::new(
                kind,
                serde_json::json!({"transcript_event": transcript_event}),
            );
            event.actor = actor;
            let source_event_id = source_event_id(&event.payload);
            let tool_call_id = tool_call_id(&event.payload);
            apply_identity(&mut event, config, source_event_id, None, tool_call_id)?;
            Ok(event)
        }
        TranscriptMutation::MessagesReplaced {
            messages,
            summary,
            source_event_ids,
        } => {
            let mut event = AppendEvent::new(
                SessionEventKind::Compaction,
                serde_json::json!({
                    "messages": messages,
                    "summary": summary,
                    "source_event_ids": source_event_ids,
                }),
            );
            apply_identity(&mut event, config, None, None, None)?;
            Ok(event)
        }
        TranscriptMutation::MessageRemoved {
            source_event_id,
            raw_message,
        } => {
            let mut event = AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "message_removed".to_string(),
                },
                serde_json::json!({
                    "source_event_id": source_event_id,
                    "raw_message": raw_message,
                }),
            );
            apply_identity(&mut event, config, Some(source_event_id), None, None)?;
            Ok(event)
        }
    }
}

fn event_kind_for_transcript(event: &serde_json::Value, message_default: bool) -> SessionEventKind {
    match json_string(event, "kind").as_deref() {
        Some("tool_result") => SessionEventKind::ToolResult,
        Some("plan") => SessionEventKind::Plan,
        Some("compaction") => SessionEventKind::Compaction,
        Some("system_reminder") => SessionEventKind::SystemReminder,
        Some("reminder") => SessionEventKind::Reminder,
        Some("permission_decision") => SessionEventKind::PermissionDecision,
        Some("message") if message_default => SessionEventKind::Message,
        Some(kind) => SessionEventKind::Custom {
            custom_type: kind.to_string(),
        },
        None if message_default => SessionEventKind::Message,
        None => SessionEventKind::Custom {
            custom_type: "transcript_event".to_string(),
        },
    }
}

fn apply_identity(
    event: &mut AppendEvent,
    config: &JournalConfig,
    source_event_id: Option<String>,
    message_id: Option<String>,
    tool_call_id: Option<String>,
) -> Result<(), VmError> {
    let mut identity = EventIdentity::new()
        .with(EventIdentityField::RunId, config.run_id.clone())
        .and_then(|identity| identity.with(EventIdentityField::TurnId, config.turn_id.clone()))
        .map_err(|error| VmError::Runtime(format!("agent transcript journal identity: {error}")))?;
    for (field, value) in [
        (EventIdentityField::SourceEventId, source_event_id),
        (EventIdentityField::MessageId, message_id),
        (EventIdentityField::ToolCallId, tool_call_id),
    ] {
        if let Some(value) = value {
            identity = identity.with(field, value).map_err(|error| {
                VmError::Runtime(format!("agent transcript journal identity: {error}"))
            })?;
        }
    }
    identity
        .apply_to_headers(&mut event.headers)
        .map_err(|error| VmError::Runtime(format!("agent transcript journal identity: {error}")))
}

fn hydrate_events(events: Vec<harn_session_store::StoredEvent>) -> HydratedTranscript {
    let mut messages: Vec<(Option<String>, serde_json::Value)> = Vec::new();
    let mut summary = None;
    for event in events {
        if matches!(
            event.kind,
            SessionEventKind::Message | SessionEventKind::ToolCall | SessionEventKind::ToolResult
        ) {
            if let Some(message) = event.payload.get("raw_message").cloned() {
                messages.push((event.headers.get("source_event_id").cloned(), message));
                continue;
            }
        }
        match &event.kind {
            SessionEventKind::Compaction => {
                if let Some(replaced) = event
                    .payload
                    .get("messages")
                    .and_then(|value| value.as_array())
                {
                    let source_event_ids = event
                        .payload
                        .get("source_event_ids")
                        .and_then(serde_json::Value::as_array);
                    messages = replaced
                        .iter()
                        .enumerate()
                        .map(|(index, message)| {
                            let source_event_id = source_event_ids
                                .and_then(|ids| ids.get(index))
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string);
                            (source_event_id, message.clone())
                        })
                        .collect();
                }
                summary = event
                    .payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
            SessionEventKind::Custom { custom_type } if custom_type == "message_removed" => {
                if let Some(source_event_id) = event
                    .payload
                    .get("source_event_id")
                    .and_then(serde_json::Value::as_str)
                {
                    if let Some(index) = messages
                        .iter()
                        .rposition(|(source, _)| source.as_deref() == Some(source_event_id))
                    {
                        messages.remove(index);
                    } else if let Some(raw_message) = event.payload.get("raw_message") {
                        if let Some(index) = messages
                            .iter()
                            .rposition(|(_, message)| message == raw_message)
                        {
                            messages.remove(index);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let (mut source_event_ids, mut messages): (Vec<_>, Vec<_>) = messages.into_iter().unzip();
    if let Some(summary_text) = summary.as_deref() {
        let summary_is_present = messages.first().is_some_and(|message| {
            message.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && message.get("content").and_then(serde_json::Value::as_str) == Some(summary_text)
        });
        if !summary_is_present {
            messages.insert(
                0,
                serde_json::json!({"role": "user", "content": summary_text}),
            );
            source_event_ids.insert(0, None);
        }
    }
    HydratedTranscript {
        messages,
        source_event_ids,
    }
}

fn source_event_id(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("transcript_event")
        .and_then(|event| json_string(event, "id"))
}

fn message_id(payload: &serde_json::Value) -> Option<String> {
    let message = payload.get("raw_message")?;
    json_string(message, "message_id").or_else(|| json_string(message, "messageId"))
}

fn tool_call_id(payload: &serde_json::Value) -> Option<String> {
    let message = payload.get("raw_message");
    let direct = message.and_then(|message| {
        json_string(message, "tool_call_id")
            .or_else(|| json_string(message, "tool_use_id"))
            .or_else(|| json_string(message, "toolUseId"))
    });
    if direct.is_some() {
        return direct;
    }
    let calls = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(serde_json::Value::as_array)?;
    (calls.len() == 1)
        .then(|| calls.first().and_then(|call| json_string(call, "id")))
        .flatten()
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    fn options(root: &std::path::Path) -> DictMap {
        let mut options = DictMap::new();
        options.put_str("root", root.to_string_lossy().as_ref());
        options
    }

    fn transcript_event(id: &str, kind: &str, role: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "kind": kind,
            "role": role,
            "visibility": "internal",
            "text": "",
        })
    }

    #[tokio::test]
    async fn journal_persists_identity_hydrates_and_replays_replacements() {
        reset();
        let root = tempfile::tempdir().expect("temp root");
        let options = options(root.path());
        let session_id = "journal-round-trip";

        let initial = hydrate_and_configure(
            session_id,
            &options,
            "run-first".to_string(),
            "turn-first".to_string(),
        )
        .await
        .expect("configure initial journal");
        assert!(initial.messages.is_empty());

        enqueue_message(
            session_id,
            transcript_event("event-user", "message", "user"),
            serde_json::json!({
                "role": "user",
                "content": "persist me",
                "messageId": "message-user",
            }),
        );
        enqueue_message(
            session_id,
            transcript_event("event-tool", "tool_result", "tool_result"),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "tool-1",
                "content": "tool output",
            }),
        );
        flush(session_id).await.expect("flush messages");

        let store = session_store::open_canonical_agent_session(root.path(), session_id)
            .await
            .expect("open canonical store");
        let events = session_store::read_all_events(&store, session_id)
            .await
            .expect("read canonical events");
        assert_eq!(events.len(), 2);
        let identity = events[0].identity().expect("identity");
        assert_eq!(identity.get(EventIdentityField::RunId), Some("run-first"));
        assert_eq!(identity.get(EventIdentityField::TurnId), Some("turn-first"));
        assert_eq!(
            identity.get(EventIdentityField::SourceEventId),
            Some("event-user")
        );
        assert_eq!(
            identity.get(EventIdentityField::MessageId),
            Some("message-user")
        );
        assert_eq!(events[1].kind, SessionEventKind::ToolResult);
        assert_eq!(
            events[1].headers.get("tool_call_id"),
            Some(&"tool-1".to_string())
        );

        clear(session_id);
        let hydrated = hydrate_and_configure(
            session_id,
            &options,
            "run-second".to_string(),
            "turn-second".to_string(),
        )
        .await
        .expect("hydrate persisted messages");
        assert_eq!(hydrated.messages.len(), 2);
        assert_eq!(hydrated.messages[0]["content"], "persist me");
        assert_eq!(
            hydrated.source_event_ids,
            vec![
                Some("event-user".to_string()),
                Some("event-tool".to_string()),
            ]
        );

        enqueue_message_removed(
            session_id,
            "event-tool".to_string(),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "tool-1",
                "content": "tool output",
            }),
        );
        flush(session_id).await.expect("flush durable removal");
        clear(session_id);

        let removed = hydrate_and_configure(
            session_id,
            &options,
            "run-removal".to_string(),
            "turn-removal".to_string(),
        )
        .await
        .expect("hydrate durable removal");
        assert_eq!(removed.messages.len(), 1);

        enqueue_messages_replaced(
            session_id,
            vec![serde_json::json!({"role": "assistant", "content": "retained turn"})],
            Some("compacted context".to_string()),
            vec![Some("compacted-assistant".to_string())],
        );
        flush(session_id).await.expect("flush replacement");
        clear(session_id);

        let compacted = hydrate_and_configure(
            session_id,
            &options,
            "run-third".to_string(),
            "turn-third".to_string(),
        )
        .await
        .expect("hydrate replacement");
        assert_eq!(compacted.messages.len(), 2);
        assert_eq!(compacted.messages[0]["content"], "compacted context");
        assert_eq!(compacted.messages[1]["content"], "retained turn");
        assert_eq!(
            compacted.source_event_ids,
            vec![None, Some("compacted-assistant".to_string())]
        );

        enqueue_message_removed(
            session_id,
            "new-in-memory-event".to_string(),
            serde_json::json!({"role": "assistant", "content": "retained turn"}),
        );
        flush(session_id).await.expect("flush fallback removal");
        clear(session_id);

        let removed_compacted = hydrate_and_configure(
            session_id,
            &options,
            "run-fallback".to_string(),
            "turn-fallback".to_string(),
        )
        .await
        .expect("hydrate fallback removal");
        assert_eq!(removed_compacted.messages.len(), 1);
        assert_eq!(
            removed_compacted.messages[0]["content"],
            "compacted context"
        );
        clear(session_id);
        reset();
    }

    #[test]
    fn singular_tool_call_message_uses_tool_call_row() {
        let event = append_event_for_mutation(
            &JournalConfig {
                root: PathBuf::new(),
                run_id: "run-tool".to_string(),
                turn_id: "turn-tool".to_string(),
            },
            TranscriptMutation::MessageAdded {
                transcript_event: transcript_event("event-call", "message", "assistant"),
                raw_message: serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "tool-1", "name": "look", "arguments": {}}],
                }),
            },
        )
        .expect("map tool call");

        assert_eq!(event.kind, SessionEventKind::ToolCall);
        assert_eq!(
            event.headers.get("tool_call_id"),
            Some(&"tool-1".to_string())
        );
    }
}
