//! One journal queue, including facts produced by off-thread provider work.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

use super::{SessionEventPayload, TranscriptKind, TranscriptRecord};

#[derive(Default)]
struct QueueState {
    pending: VecDeque<SessionEventPayload>,
    sealed: bool,
}

#[derive(Default)]
pub(super) struct JournalQueue(Arc<Mutex<QueueState>>);

/// Append-only access to the existing journal, with no session lookup or store
/// handle. A provider may move this across threads; the VM still owns flushing.
#[derive(Clone)]
pub(crate) struct JournalAuditWriter(Arc<Mutex<QueueState>>);

impl JournalQueue {
    pub(super) fn audit_writer(&self) -> JournalAuditWriter {
        JournalAuditWriter(self.0.clone())
    }

    pub(super) fn is_sealed(&self) -> bool {
        self.0.lock().sealed
    }

    pub(super) fn seal(&self) {
        self.0.lock().sealed = true;
    }

    pub(super) fn enqueue(&self, payload: SessionEventPayload) {
        let mut state = self.0.lock();
        if matches!(
            &payload,
            SessionEventPayload::Transcript(record)
                if record.transcript_event.kind == Some(TranscriptKind::AgentRunTerminal)
        ) {
            // Closing and enqueueing share the same lock: an off-thread fact
            // cannot be accepted after the terminal but before a separate flag.
            state.sealed = true;
        }
        state.pending.push_back(payload);
    }

    pub(super) fn snapshot(&self) -> Vec<SessionEventPayload> {
        self.0.lock().pending.iter().cloned().collect()
    }

    pub(super) fn record_persisted(&self, count: usize) {
        let mut state = self.0.lock();
        let count = count.min(state.pending.len());
        state.pending.drain(..count);
    }
}

impl JournalAuditWriter {
    pub(crate) fn append(&self, event: serde_json::Value) -> Result<(), String> {
        let record = TranscriptRecord::new(event, None);
        if record.transcript_event.kind.is_none()
            || record.transcript_event.kind == Some(TranscriptKind::AgentRunTerminal)
        {
            return Err("agent transcript audit writer requires a non-terminal event".into());
        }
        let mut state = self.0.lock();
        if state.sealed {
            return Err("agent transcript audit writer: journal is closed".into());
        }
        state
            .pending
            .push_back(SessionEventPayload::Transcript(record));
        Ok(())
    }
}
