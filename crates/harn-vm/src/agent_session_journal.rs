//! Durable mutation journal for live agent transcripts.
//!
//! Transcript mutation remains synchronous and VM-local. This module records
//! the exact mutations, then flushes them through the canonical session-store
//! at existing async agent-loop boundaries. `AgentEvent` sinks stay strictly
//! observability-only: a filtered or failed sink can never alter durability.

use harn_session_store::{
    AppendEvent, EventIdentity, EventIdentityField, SessionEventKind, SessionType,
    SessionWriteLease,
};

use crate::agent_sessions::session_event::{
    CompactionRecord, MessageRemovedRecord, SessionEventPayload, TranscriptKind, TranscriptRecord,
    SESSION_EVENT_SCHEMA_HEADER, SESSION_EVENT_SCHEMA_VERSION,
};
use crate::stdlib::session_store;
use crate::value::{DictMap, VmError};

mod queue;
pub(crate) use queue::JournalAuditWriter;
use queue::JournalQueue;

const WORKFLOW_LEARNING_ELIGIBILITY_HEADER: &str = "harn.workflow_learning.eligibility";
const WORKFLOW_LEARNING_ELIGIBLE: &str = "eligible";
const WORKFLOW_LEARNING_EXCLUDED: &str = "excluded";

/// The store a flush appends to and every queued mutation as its event,
/// oldest first.
pub(crate) type PendingJournalBatch = (
    crate::stdlib::canonical_store::CanonicalStore,
    Vec<AppendEvent>,
);

#[derive(Clone)]
struct JournalConfig {
    store: crate::stdlib::canonical_store::CanonicalStore,
    run_id: String,
    turn_id: String,
    workflow_learning_eligibility: String,
}

pub(crate) struct JournalState {
    config: JournalConfig,
    pending: JournalQueue,
    first_event_id: Option<u64>,
    execution_id: Option<String>,
    task_id: Option<String>,
    owns_session: bool,
    lifecycle_reservation: Option<crate::agent_lifecycle_cleanup::LifecycleReservation>,
    /// Held across a whole flush. A flush reads the queue head, awaits its
    /// append, then pops it, so two interleaved flushes (sibling tool calls in
    /// `parallel settle`) would each append the same head.
    flush_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    _writer_lease: SessionWriteLease,
}

impl JournalState {
    pub(crate) fn flush_lock(&self) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.flush_lock.clone()
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.config.run_id
    }

    pub(crate) fn store(&self) -> crate::stdlib::canonical_store::CanonicalStore {
        self.config.store.clone()
    }

    pub(crate) fn first_event_id(&self) -> Option<u64> {
        self.first_event_id
    }

    pub(crate) fn terminal_queued(&self) -> bool {
        self.pending.is_sealed()
    }

    pub(crate) fn audit_writer(&self) -> JournalAuditWriter {
        self.pending.audit_writer()
    }

    pub(crate) fn claim_task(
        &mut self,
        execution_id: &str,
        session_id: &str,
        task_id: String,
        owns_session: bool,
    ) -> Result<(), VmError> {
        let reservation = crate::agent_lifecycle_cleanup::reserve(
            execution_id,
            session_id,
            self.run_id(),
            &task_id,
        )?;
        self.execution_id = Some(execution_id.to_string());
        self.task_id = Some(task_id);
        self.owns_session = owns_session;
        self.lifecycle_reservation = Some(reservation);
        Ok(())
    }

    pub(crate) fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub(crate) fn execution_id(&self) -> Option<&str> {
        self.execution_id.as_deref()
    }

    pub(crate) fn owns_session(&self) -> bool {
        self.owns_session
    }

    pub(crate) fn is_claimed(&self) -> bool {
        self.lifecycle_reservation.is_some()
    }

    #[cfg(test)]
    pub(crate) fn next_event(
        &self,
    ) -> Result<Option<(crate::stdlib::canonical_store::CanonicalStore, AppendEvent)>, VmError>
    {
        self.pending
            .snapshot()
            .into_iter()
            .next()
            .map(|payload| {
                append_event_for_payload(&self.config, payload)
                    .map(|event| (self.config.store.clone(), event))
            })
            .transpose()
    }

    /// Every queued mutation, oldest first, as the events one flush appends.
    pub(crate) fn pending_events(&self) -> Result<PendingJournalBatch, VmError> {
        let events = self
            .pending
            .snapshot()
            .into_iter()
            .map(|payload| append_event_for_payload(&self.config, payload))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((self.config.store.clone(), events))
    }

    /// Dequeue the oldest mutations, which the store has just persisted as
    /// `event_ids` in the same order. Mutations queued during the append
    /// stay behind them.
    pub(crate) fn record_persisted_events(&mut self, event_ids: &[u64]) {
        if let Some(first) = event_ids.first() {
            self.first_event_id.get_or_insert(*first);
        }
        self.pending.record_persisted(event_ids.len());
    }
}

impl Drop for JournalState {
    fn drop(&mut self) {
        self.pending.seal();
    }
}

#[derive(Default)]
pub(crate) struct HydratedTranscript {
    pub messages: Vec<serde_json::Value>,
    pub source_event_ids: Vec<Option<String>>,
    pub summary: Option<String>,
}

impl HydratedTranscript {
    pub(crate) fn restore(&self, session_id: &str) -> Result<(), String> {
        crate::agent_sessions::replace_messages_with_summary(
            session_id,
            &self.messages,
            self.summary.as_deref(),
        )?;
        crate::agent_sessions::restore_message_event_ids(session_id, &self.source_event_ids)
    }
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
    EventIdentity::new()
        .with(EventIdentityField::RunId, run_id.clone())
        .map_err(|error| VmError::Runtime(format!("agent transcript journal identity: {error}")))?;
    let state_dir = session_store::canonical_store_state_dir(Some(options))?;
    let parent_session_id = options
        .get("parent_session_id")
        .and_then(|value| match value {
            crate::value::VmValue::String(value) if !value.trim().is_empty() => {
                Some(value.to_string())
            }
            _ => None,
        });
    let session_type = match options.get("session_type") {
        Some(crate::value::VmValue::String(value)) if value.as_str() == "subagent" => {
            SessionType::Subagent
        }
        Some(crate::value::VmValue::String(value)) if value.as_str() == "scheduled" => {
            SessionType::Scheduled
        }
        _ => SessionType::User,
    };
    let workflow_learning_eligibility = workflow_learning_eligibility(options)?;
    // Claim the writer slot before opening or creating the session. Maintenance
    // owns the project lease exclusively, so acquiring in this order closes
    // the window where cleanup could delete a session during preparation.
    let writer_lease = SessionWriteLease::try_acquire(state_dir.as_path(), session_id)
        .map_err(|error| session_store::session_lease_error("agent transcript journal", error))?;
    let store = session_store::open_canonical_agent_session(
        &state_dir,
        session_id,
        parent_session_id,
        session_type,
    )
    .await?;
    let events = session_store::read_all_events(&store, session_id).await?;
    Ok(PreparedJournal {
        transcript: hydrate_events(events),
        state: JournalState {
            config: JournalConfig {
                store,
                run_id,
                turn_id,
                workflow_learning_eligibility,
            },
            pending: JournalQueue::default(),
            first_event_id: None,
            execution_id: None,
            task_id: None,
            owns_session: false,
            lifecycle_reservation: None,
            flush_lock: std::sync::Arc::default(),
            _writer_lease: writer_lease,
        },
    })
}

/// Closed per-run provenance for whether a completed agent turn may become
/// reusable-workflow evidence. It is written on each persisted event (and
/// therefore on the terminal event), so a later coding turn in a long-lived
/// conversation does not inherit the eligibility of an earlier ask/plan turn.
fn workflow_learning_eligibility(options: &DictMap) -> Result<String, VmError> {
    let eligibility = match options.get("workflow_learning_eligibility") {
        None => WORKFLOW_LEARNING_EXCLUDED,
        Some(crate::value::VmValue::String(value)) if value.as_str() == WORKFLOW_LEARNING_ELIGIBLE => {
            WORKFLOW_LEARNING_ELIGIBLE
        }
        Some(crate::value::VmValue::String(value)) if value.as_str() == WORKFLOW_LEARNING_EXCLUDED => {
            WORKFLOW_LEARNING_EXCLUDED
        }
        Some(crate::value::VmValue::String(value)) => {
            return Err(VmError::Runtime(format!(
                "agent_loop: options.workflow_learning_eligibility must be 'eligible' or 'excluded'; got '{value}'"
            )))
        }
        Some(_) => {
            return Err(VmError::Runtime(
                "agent_loop: options.workflow_learning_eligibility must be 'eligible' or 'excluded'"
                    .to_string(),
            ))
        }
    };
    Ok(eligibility.to_string())
}

pub(crate) fn enqueue_message(
    journal: &mut Option<JournalState>,
    transcript_event: serde_json::Value,
    raw_message: serde_json::Value,
) {
    enqueue(
        journal,
        SessionEventPayload::Transcript(TranscriptRecord::new(transcript_event, Some(raw_message))),
    );
}

pub(crate) fn enqueue_audit_event(
    journal: &mut Option<JournalState>,
    transcript_event: serde_json::Value,
) {
    enqueue(
        journal,
        SessionEventPayload::Transcript(TranscriptRecord::new(transcript_event, None)),
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
        SessionEventPayload::Compaction(CompactionRecord {
            messages,
            summary,
            source_event_ids,
        }),
    );
}

pub(crate) fn enqueue_message_removed(
    journal: &mut Option<JournalState>,
    source_event_id: String,
    raw_message: serde_json::Value,
) {
    enqueue(
        journal,
        SessionEventPayload::MessageRemoved(MessageRemovedRecord {
            source_event_id,
            raw_message: Some(raw_message),
        }),
    );
}

fn enqueue(journal: &mut Option<JournalState>, payload: SessionEventPayload) {
    if let Some(journal) = journal {
        journal.pending.enqueue(payload);
    }
}

/// Persist queued mutations in chronological order, all of them in one store
/// transaction. Mutations stay queued until that transaction commits, so a
/// failure leaves every one of them queued, observable, and retryable without
/// a background writer, a best-effort drop, or a duplicated prefix.
pub(crate) async fn flush(session_id: &str) -> Result<(), VmError> {
    let Some(lock) = crate::agent_sessions::journal_flush_lock(session_id) else {
        return Ok(());
    };
    let _flushing = lock.lock().await;
    let Some((store, events)) = crate::agent_sessions::pending_journal_events(session_id)? else {
        return Ok(());
    };
    if events.is_empty() {
        return Ok(());
    }
    let stored = store
        .append_batch(session_id, events)
        .await
        .map_err(|error| VmError::Runtime(format!("agent transcript journal: {error}")))?;
    let event_ids: Vec<u64> = stored.iter().map(|event| event.event_id).collect();
    crate::agent_sessions::record_persisted_journal_events(session_id, &event_ids);
    Ok(())
}

fn append_event_for_payload(
    config: &JournalConfig,
    payload: SessionEventPayload,
) -> Result<AppendEvent, VmError> {
    let (actor, source_event_id, message_id, tool_call_id) = match &payload {
        SessionEventPayload::Transcript(record) => (
            record
                .transcript_event
                .role
                .clone()
                .filter(|role| !role.trim().is_empty()),
            record
                .transcript_event
                .id
                .clone()
                .filter(|id| !id.trim().is_empty()),
            record.message_id(),
            record.journal_tool_call_id(),
        ),
        SessionEventPayload::Compaction(_) => (None, None, None, None),
        SessionEventPayload::MessageRemoved(record) => {
            (None, Some(record.source_event_id.clone()), None, None)
        }
    };
    let mut event = AppendEvent::new(payload.store_kind(), payload.to_json());
    event.actor = actor;
    apply_identity(
        &mut event,
        config,
        source_event_id,
        message_id,
        tool_call_id,
    )?;
    Ok(event)
}

pub(crate) fn messages_replaced_event(
    messages: Vec<serde_json::Value>,
    summary: Option<String>,
    source_event_ids: Vec<Option<String>>,
) -> AppendEvent {
    let payload = SessionEventPayload::Compaction(CompactionRecord {
        messages,
        summary,
        source_event_ids,
    });
    let mut event = AppendEvent::new(payload.store_kind(), payload.to_json());
    event.headers.insert(
        SESSION_EVENT_SCHEMA_HEADER.to_string(),
        SESSION_EVENT_SCHEMA_VERSION.to_string(),
    );
    event
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
        .map_err(|error| VmError::Runtime(format!("agent transcript journal identity: {error}")))?;
    event.headers.insert(
        WORKFLOW_LEARNING_ELIGIBILITY_HEADER.to_string(),
        config.workflow_learning_eligibility.clone(),
    );
    event.headers.insert(
        SESSION_EVENT_SCHEMA_HEADER.to_string(),
        SESSION_EVENT_SCHEMA_VERSION.to_string(),
    );
    Ok(())
}

pub(crate) fn hydrate_events(events: Vec<harn_session_store::StoredEvent>) -> HydratedTranscript {
    hydrate_events_with_publications(&events).0
}

/// Replay once with full-session indices, including compaction and removal.
/// Reporting may retain an admitted message even after later compaction.
pub(crate) fn hydrate_events_with_publications(
    events: &[harn_session_store::StoredEvent],
) -> (
    HydratedTranscript,
    std::collections::BTreeMap<String, crate::llm::assistant_publication::PublishedMessage>,
) {
    let mut hydration = TranscriptHydration::default();
    for event in events {
        hydration.absorb(event);
    }
    hydration.finish()
}

/// Incremental journal replay for bounded store pages. Retains the current
/// transcript and sealed admissions, never a second copy of stored events.
/// Page boundaries do not reset the session-wide receipt indices.
#[derive(Default)]
pub(crate) struct TranscriptHydration {
    messages: Vec<(Option<String>, serde_json::Value)>,
    summary: Option<String>,
    publications:
        std::collections::BTreeMap<String, crate::llm::assistant_publication::PublishedMessage>,
}

impl TranscriptHydration {
    pub(crate) fn absorb(&mut self, event: &harn_session_store::StoredEvent) {
        let Self {
            messages,
            summary,
            publications,
        } = self;
        let source_event_id = event.headers.get("source_event_id");
        match SessionEventPayload::decode(event) {
            Some(SessionEventPayload::Transcript(record)) => {
                if matches!(
                    event.kind,
                    SessionEventKind::Message
                        | SessionEventKind::ToolCall
                        | SessionEventKind::ToolResult
                ) {
                    if let Some(message) = record.raw_message {
                        messages.push((source_event_id.cloned(), message));
                        return;
                    }
                }
                if record
                    .transcript_event
                    .is(TranscriptKind::AssistantPublication)
                {
                    publications.extend(crate::llm::assistant_publication::replay(
                        messages,
                        &record.transcript_event.metadata_json(),
                        source_event_id.map(String::as_str),
                    ));
                }
            }
            Some(SessionEventPayload::Compaction(record)) => {
                *messages = record
                    .messages
                    .into_iter()
                    .enumerate()
                    .map(|(index, message)| {
                        (
                            record.source_event_ids.get(index).cloned().flatten(),
                            message,
                        )
                    })
                    .collect();
                *summary = record.summary;
            }
            Some(SessionEventPayload::MessageRemoved(record)) => {
                if let Some(index) = messages
                    .iter()
                    .rposition(|(source, _)| source.as_deref() == Some(&record.source_event_id))
                {
                    messages.remove(index);
                } else if let Some(raw_message) = &record.raw_message {
                    if let Some(index) = messages
                        .iter()
                        .rposition(|(_, message)| message == raw_message)
                    {
                        messages.remove(index);
                    }
                }
            }
            None => {}
        }
    }

    pub(crate) fn finish(
        self,
    ) -> (
        HydratedTranscript,
        std::collections::BTreeMap<String, crate::llm::assistant_publication::PublishedMessage>,
    ) {
        let Self {
            messages,
            summary,
            publications,
        } = self;
        let (mut source_event_ids, mut messages): (Vec<_>, Vec<_>) = messages.into_iter().unzip();
        if let Some(summary_text) = summary.as_deref() {
            let summary_is_present = messages.first().is_some_and(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("user")
                    && message.get("content").and_then(serde_json::Value::as_str)
                        == Some(summary_text)
            });
            if !summary_is_present {
                messages.insert(
                    0,
                    serde_json::json!({"role": "user", "content": summary_text}),
                );
                source_event_ids.insert(0, None);
            }
        }
        (
            HydratedTranscript {
                messages,
                source_event_ids,
                summary,
            },
            publications,
        )
    }
}

#[cfg(test)]
#[path = "agent_session_journal_public_timeline_tests.rs"]
mod public_timeline_tests;

#[cfg(test)]
#[path = "agent_session_journal_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_session_journal/journal_handoff_tests.rs"]
mod journal_handoff_tests;
