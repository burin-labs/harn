//! Keep private drafts deferred until the journal owner seals their admission.

use std::collections::BTreeMap;

use harn_session_store::{SessionEventKind, StoredEvent};
use serde_json::Value;

use crate::agent_events::AgentEvent;
use crate::llm::assistant_publication::{is_visible, PublishedMessage};
use crate::orchestration::AgentSessionReplayEvent;

pub(super) struct RestoreProjection {
    event: AgentSessionReplayEvent,
    admission: Option<(String, Value)>,
}

impl RestoreProjection {
    pub(super) fn from_stored(session_id: &str, stored: &StoredEvent) -> Option<Self> {
        if matches!(stored.kind, SessionEventKind::Message) {
            if let Some(raw) = stored.payload.get("raw_message") {
                if !is_visible(&crate::schema::json_to_vm_value(raw)) {
                    return Some(Self {
                        event: AgentSessionReplayEvent {
                            event_id: stored.event_id,
                            kind: super::stored_kind_label(&stored.kind),
                            occurred_at_ms: stored.ts_ms,
                            execution_id: None,
                            event: AgentEvent::AgentMessageChunk {
                                session_id: session_id.to_string(),
                                content: String::new(),
                                history_source_event_id: None,
                            },
                        },
                        admission: Some((
                            stored.headers.get("source_event_id")?.clone(),
                            raw.clone(),
                        )),
                    });
                }
            }
        }
        super::replay_event_from_stored(session_id, stored).map(|event| Self {
            event,
            admission: None,
        })
    }

    pub(super) fn finish(
        mut self,
        publications: &BTreeMap<String, PublishedMessage>,
    ) -> Option<AgentSessionReplayEvent> {
        if let Some((source_id, original)) = self.admission {
            let publication = publications.get(&source_id)?;
            let text = publication.text_for(&original)?;
            let AgentEvent::AgentMessageChunk {
                content,
                history_source_event_id,
                ..
            } = &mut self.event.event
            else {
                unreachable!("only deferred assistant text awaits admission");
            };
            *content = text.to_string();
            *history_source_event_id = publication.history_source_event_id.clone();
        }
        Some(self.event)
    }
}
