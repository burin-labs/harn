//! Durable mutation journal for live agent transcripts.
//!
//! Transcript mutation remains synchronous and VM-local. This module records
//! the exact mutations, then flushes them through the canonical session-store
//! at existing async agent-loop boundaries. `AgentEvent` sinks stay strictly
//! observability-only: a filtered or failed sink can never alter durability.

use std::collections::VecDeque;

use harn_session_store::{
    AppendEvent, EventIdentity, EventIdentityField, SessionEventKind, SessionStore,
    SqliteSessionStore,
};

use crate::stdlib::session_store;
use crate::value::{DictMap, VmError};

#[derive(Clone)]
struct JournalConfig {
    store: SqliteSessionStore,
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

pub(crate) struct JournalState {
    config: JournalConfig,
    pending: VecDeque<TranscriptMutation>,
}

impl JournalState {
    pub(crate) fn next_event(&self) -> Result<Option<(SqliteSessionStore, AppendEvent)>, VmError> {
        self.pending
            .front()
            .cloned()
            .map(|mutation| {
                append_event_for_mutation(&self.config, mutation)
                    .map(|event| (self.config.store.clone(), event))
            })
            .transpose()
    }

    pub(crate) fn pop_front(&mut self) {
        self.pending.pop_front();
    }
}

#[derive(Default)]
pub(crate) struct HydratedTranscript {
    pub messages: Vec<serde_json::Value>,
    pub source_event_ids: Vec<Option<String>>,
}

pub(crate) struct PreparedJournal {
    pub transcript: HydratedTranscript,
    pub state: JournalState,
}

/// Open the canonical session once and prepare its per-agent-run journal.
/// The caller installs the returned state after creating or hydrating the
/// corresponding VM session record.
pub(crate) async fn prepare(
    session_id: &str,
    options: &DictMap,
    run_id: String,
    turn_id: String,
) -> Result<PreparedJournal, VmError> {
    let state_dir = session_store::canonical_store_state_dir(Some(options))?;
    let store = session_store::open_canonical_agent_session(&state_dir, session_id).await?;
    let events = session_store::read_all_events(&store, session_id).await?;
    Ok(PreparedJournal {
        transcript: hydrate_events(events),
        state: JournalState {
            config: JournalConfig {
                store,
                run_id,
                turn_id,
            },
            pending: VecDeque::new(),
        },
    })
}

pub(crate) fn enqueue_message(
    journal: &mut Option<JournalState>,
    transcript_event: serde_json::Value,
    raw_message: serde_json::Value,
) {
    enqueue(
        journal,
        TranscriptMutation::MessageAdded {
            transcript_event,
            raw_message,
        },
    );
}

pub(crate) fn enqueue_audit_event(
    journal: &mut Option<JournalState>,
    transcript_event: serde_json::Value,
) {
    enqueue(
        journal,
        TranscriptMutation::AuditEventAdded { transcript_event },
    );
}

pub(crate) fn enqueue_messages_replaced(
    journal: &mut Option<JournalState>,
    messages: Vec<serde_json::Value>,
    summary: Option<String>,
    source_event_ids: Vec<Option<String>>,
) {
    enqueue(
        journal,
        TranscriptMutation::MessagesReplaced {
            messages,
            summary,
            source_event_ids,
        },
    );
}

pub(crate) fn enqueue_message_removed(
    journal: &mut Option<JournalState>,
    source_event_id: String,
    raw_message: serde_json::Value,
) {
    enqueue(
        journal,
        TranscriptMutation::MessageRemoved {
            source_event_id,
            raw_message,
        },
    );
}

fn enqueue(journal: &mut Option<JournalState>, mutation: TranscriptMutation) {
    if let Some(journal) = journal {
        journal.pending.push_back(mutation);
    }
}

/// Persist queued mutations in chronological order. A mutation remains queued
/// until its individual append succeeds, so a later failure is observable and
/// retryable without a background writer or a best-effort drop.
pub(crate) async fn flush(session_id: &str) -> Result<(), VmError> {
    loop {
        let Some((store, event)) = crate::agent_sessions::next_journal_event(session_id)? else {
            return Ok(());
        };
        store
            .append(session_id, event)
            .await
            .map_err(|error| VmError::Runtime(format!("agent transcript journal: {error}")))?;
        crate::agent_sessions::pop_journal_event(session_id);
    }
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
        Some("tool_call") => SessionEventKind::ToolCall,
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
    let transcript_event = payload.get("transcript_event");
    [
        payload.get("raw_message"),
        transcript_event,
        transcript_event.and_then(|event| event.get("metadata")),
    ]
    .into_iter()
    .flatten()
    .find_map(tool_call_id_from_value)
}

fn tool_call_id_from_value(value: &serde_json::Value) -> Option<String> {
    json_string(value, "tool_call_id")
        .or_else(|| json_string(value, "tool_use_id"))
        .or_else(|| json_string(value, "toolUseId"))
        .or_else(|| {
            value
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .filter(|calls| calls.len() == 1)
                .and_then(|calls| calls.first())
                .and_then(|call| json_string(call, "id"))
        })
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

    async fn prepare_test_journal(
        session_id: &str,
        options: &DictMap,
        run_id: &str,
        turn_id: &str,
    ) -> (HydratedTranscript, Option<JournalState>) {
        let prepared = prepare(session_id, options, run_id.to_string(), turn_id.to_string())
            .await
            .expect("prepare journal");
        crate::agent_sessions::open_or_create(Some(session_id.to_string()));
        (prepared.transcript, Some(prepared.state))
    }

    fn install_test_journal(session_id: &str, journal: &mut Option<JournalState>) {
        crate::agent_sessions::install_journal(
            session_id,
            journal.take().expect("prepared journal state"),
        )
        .expect("install journal");
    }

    fn mapping_config() -> JournalConfig {
        JournalConfig {
            store: SqliteSessionStore::open_in_memory().expect("in-memory store"),
            run_id: "run-tool".to_string(),
            turn_id: "turn-tool".to_string(),
        }
    }

    #[tokio::test]
    async fn journal_persists_identity_hydrates_and_replays_replacements() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let options = options(root.path());
        let session_id = "journal-round-trip";

        let (initial, mut journal) =
            prepare_test_journal(session_id, &options, "run-first", "turn-first").await;
        assert!(initial.messages.is_empty());

        enqueue_message(
            &mut journal,
            transcript_event("event-user", "message", "user"),
            serde_json::json!({
                "role": "user",
                "content": "persist me",
                "messageId": "message-user",
            }),
        );
        enqueue_message(
            &mut journal,
            transcript_event("event-tool", "tool_result", "tool_result"),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "tool-1",
                "content": "tool output",
            }),
        );
        install_test_journal(session_id, &mut journal);
        flush(session_id).await.expect("flush messages");

        let store = session_store::open_canonical_agent_session(
            &session_store::SessionStoreDir::under_root(root.path()),
            session_id,
        )
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

        crate::agent_sessions::reset_session_store();
        let (hydrated, mut journal) =
            prepare_test_journal(session_id, &options, "run-second", "turn-second").await;
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
            &mut journal,
            "event-tool".to_string(),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "tool-1",
                "content": "tool output",
            }),
        );
        install_test_journal(session_id, &mut journal);
        flush(session_id).await.expect("flush durable removal");
        crate::agent_sessions::reset_session_store();

        let (removed, mut journal) =
            prepare_test_journal(session_id, &options, "run-removal", "turn-removal").await;
        assert_eq!(removed.messages.len(), 1);

        enqueue_messages_replaced(
            &mut journal,
            vec![serde_json::json!({"role": "assistant", "content": "retained turn"})],
            Some("compacted context".to_string()),
            vec![Some("compacted-assistant".to_string())],
        );
        install_test_journal(session_id, &mut journal);
        flush(session_id).await.expect("flush replacement");
        crate::agent_sessions::reset_session_store();

        let (compacted, mut journal) =
            prepare_test_journal(session_id, &options, "run-third", "turn-third").await;
        assert_eq!(compacted.messages.len(), 2);
        assert_eq!(compacted.messages[0]["content"], "compacted context");
        assert_eq!(compacted.messages[1]["content"], "retained turn");
        assert_eq!(
            compacted.source_event_ids,
            vec![None, Some("compacted-assistant".to_string())]
        );

        enqueue_message_removed(
            &mut journal,
            "new-in-memory-event".to_string(),
            serde_json::json!({"role": "assistant", "content": "retained turn"}),
        );
        install_test_journal(session_id, &mut journal);
        flush(session_id).await.expect("flush fallback removal");
        crate::agent_sessions::reset_session_store();

        let (removed_compacted, _) =
            prepare_test_journal(session_id, &options, "run-fallback", "turn-fallback").await;
        assert_eq!(removed_compacted.messages.len(), 1);
        assert_eq!(
            removed_compacted.messages[0]["content"],
            "compacted context"
        );
        crate::agent_sessions::reset_session_store();
    }

    #[tokio::test]
    async fn failed_append_keeps_the_mutation_queued() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let options = options(root.path());
        let session_id = "journal-failed-append";
        let prepared = prepare(
            session_id,
            &options,
            "run-failure".to_string(),
            "turn-failure".to_string(),
        )
        .await
        .expect("prepare journal");
        let store = prepared.state.config.store.clone();
        crate::agent_sessions::open_or_create(Some(session_id.to_string()));
        let mut journal = Some(prepared.state);
        enqueue_message(
            &mut journal,
            transcript_event("event-failure", "message", "user"),
            serde_json::json!({"role": "user", "content": "must remain queued"}),
        );
        install_test_journal(session_id, &mut journal);
        store
            .close(session_id)
            .await
            .expect("close canonical session");

        let error = flush(session_id)
            .await
            .expect_err("append to a closed session must fail");
        assert!(error.to_string().contains("closed"));
        assert!(
            crate::agent_sessions::next_journal_event(session_id)
                .expect("inspect queued mutation")
                .is_some(),
            "a failed append must not discard its pending mutation"
        );
        crate::agent_sessions::reset_session_store();
    }

    #[tokio::test]
    async fn active_run_identity_cannot_be_overwritten() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let options = options(root.path());
        let session_id = crate::agent_sessions::open_or_create(Some("journal-active-run".into()));
        let first = prepare(
            &session_id,
            &options,
            "run-first".to_string(),
            "turn-first".to_string(),
        )
        .await
        .expect("prepare first journal");
        let second = prepare(
            &session_id,
            &options,
            "run-second".to_string(),
            "turn-second".to_string(),
        )
        .await
        .expect("prepare second journal");

        crate::agent_sessions::install_journal(&session_id, first.state)
            .expect("install first journal");
        let error = crate::agent_sessions::install_journal(&session_id, second.state)
            .expect_err("a second active run must not replace the first run identity");
        assert!(error.to_string().contains("already has an active journal"));

        crate::agent_sessions::reset_session_store();
    }

    #[test]
    fn singular_tool_call_message_uses_tool_call_row() {
        let event = append_event_for_mutation(
            &mapping_config(),
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

    #[test]
    fn tool_lifecycle_event_uses_tool_call_row_and_metadata_identity() {
        let event = append_event_for_mutation(
            &mapping_config(),
            TranscriptMutation::AuditEventAdded {
                transcript_event: serde_json::json!({
                    "id": "event-call",
                    "kind": "tool_call",
                    "role": "assistant",
                    "metadata": {"tool_call_id": "tool-1", "tool_name": "look"},
                }),
            },
        )
        .expect("map lifecycle tool call");

        assert_eq!(event.kind, SessionEventKind::ToolCall);
        assert_eq!(
            event.headers.get("tool_call_id"),
            Some(&"tool-1".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generic_agent_loop_persists_canonical_transcript_lifecycle() {
        crate::agent_sessions::reset_session_store();
        crate::reset_thread_local_state();
        let root = tempfile::tempdir().expect("temp root");
        let root_literal = serde_json::to_string(
            root.path()
                .to_str()
                .expect("temporary path must be valid UTF-8"),
        )
        .expect("serialize root literal");
        let source = format!(
            r###"
pipeline main(task) {{
  llm_mock({{text: "", tool_calls: [{{id: "live-call", name: "noop", arguments: {{}}}}]}})
  llm_mock({{text: "##DONE##"}})
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "noop",
    "Return a deterministic result.",
    {{
      handler: {{ _args -> "ok" }},
      parameters: {{}},
      returns: {{type: "string"}},
    }},
  )
  const result = agent_loop(
    "Use noop, then finish.",
    nil,
    {{
      provider: "mock",
      model: "mock",
      root: {root_literal},
      session_id: "live-journal-agent-loop",
      tools: tools,
      tool_format: "native",
      loop_until_done: true,
      max_iterations: 4,
    }},
  )
  __io_println(result.status)
}}
"###,
        );
        let chunk = crate::compile_source(&source).expect("compile agent loop");
        let local = tokio::task::LocalSet::new();
        let output = local
            .run_until(async {
                let mut vm = crate::Vm::new();
                crate::register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("run agent loop");
                vm.output().to_string()
            })
            .await;
        assert!(output.lines().any(|line| line.ends_with("done")));

        let session_id = "live-journal-agent-loop";
        let store = session_store::open_canonical_agent_session(
            &session_store::SessionStoreDir::under_root(root.path()),
            session_id,
        )
        .await
        .expect("open canonical store");
        let events = session_store::read_all_events(&store, session_id)
            .await
            .expect("read canonical events");

        let user_message = events
            .iter()
            .find(|event| {
                event.kind == SessionEventKind::Message
                    && event.payload["raw_message"]["role"] == "user"
            })
            .expect("user message must be durable");
        let run_id = user_message
            .headers
            .get("run_id")
            .expect("user message must carry a run identity");
        let turn_id = user_message
            .headers
            .get("turn_id")
            .expect("user message must carry a turn identity");
        assert!(!run_id.is_empty());
        assert!(!turn_id.is_empty());

        let assistant_tool_call = events
            .iter()
            .find(|event| {
                event.kind == SessionEventKind::ToolCall
                    && event.payload["raw_message"]["role"] == "assistant"
            })
            .expect("assistant tool call must be durable");
        let tool_call_id = assistant_tool_call
            .headers
            .get("tool_call_id")
            .expect("assistant tool call must carry a correlation id");
        assert!(!tool_call_id.is_empty());
        assert!(events.iter().any(|event| {
            event.kind == SessionEventKind::ToolCall
                && event.payload["transcript_event"]["kind"] == "tool_call"
                && event.headers.get("tool_call_id") == Some(tool_call_id)
        }));
        assert!(events.iter().any(|event| {
            event.kind == SessionEventKind::ToolResult
                && event.headers.get("tool_call_id") == Some(tool_call_id)
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                SessionEventKind::Custom { custom_type } if custom_type == "tool_call_update"
            ) && event.headers.get("tool_call_id") == Some(tool_call_id)
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                SessionEventKind::Custom { custom_type } if custom_type == "agent_run_terminal"
            )
        }));
        assert!(events.iter().all(|event| {
            event.headers.get("run_id") == Some(run_id)
                && event.headers.get("turn_id") == Some(turn_id)
        }));

        crate::agent_sessions::reset_session_store();
        crate::reset_thread_local_state();
    }
}
