//! Closed transcripts are data; reopening never retains runnable authority.

use super::*;
use harn_session_store::{SessionMeta, SessionStore};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptCheckpoint {
    event_count: usize,
    last_event_id: Option<u64>,
    chain_root_hash: Option<String>,
}

impl From<&SessionMeta> for TranscriptCheckpoint {
    fn from(meta: &SessionMeta) -> Self {
        Self {
            event_count: meta.event_count,
            last_event_id: meta.last_event_id,
            chain_root_hash: meta.chain_root_hash.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClosedTranscript {
    id: String,
    token: String,
    state_dir: crate::agent_session_journal::CanonicalStoreDir,
    checkpoint: TranscriptCheckpoint,
    transcript: VmValue,
    text_tool_call_seq: u64,
    pub(crate) goal: SessionGoalState,
}

/// A candidate becomes reusable only after normal session retirement succeeds.
pub struct ClosedSessionContinuation(ClosedTranscript);

impl ClosedSessionContinuation {
    pub fn last_event_id(&self) -> Option<u64> {
        self.0.checkpoint.last_event_id
    }

    pub fn commit(self) -> String {
        let token = self.0.token.clone();
        *active_session_runtime().closed_transcript.write() = Some(self.0);
        token
    }

    pub fn restore(self) -> Result<(), String> {
        let cached = self.0;
        SESSIONS.with(|sessions| {
            let mut sessions = sessions.borrow_mut();
            let state = sessions
                .get_mut(&cached.id)
                .ok_or_else(|| "continuation requires fresh session admission".to_string())?;
            if state.transcript_journal.is_some() {
                return Err("continuation cannot replace an active run".into());
            }
            state.replace_transcript(cached.transcript)?;
            state.text_tool_call_seq = state.text_tool_call_seq.max(cached.text_tool_call_seq);
            state.goal = cached.goal;
            state.continuation_checkpoint = Some(cached.checkpoint);
            Ok(())
        })
    }
}

/// Capture the last durable transcript without retaining controls or grants.
/// Only the most recently closed transcript is retained, bounding idle retention
/// to one already budgeted session per runtime.
pub async fn prepare_closed_session_continuation(
    id: &str,
) -> Result<Option<ClosedSessionContinuation>, crate::value::VmError> {
    let state_dir = SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let state = sessions.get(id)?;
        if state.transcript_journal.is_some() || state.reset_in_flight {
            return None;
        }
        state.canonical_store_dir.clone()
    });
    let Some(state_dir) = state_dir else {
        return Ok(None);
    };
    if !active_session_runtime()
        .closed_transcript
        .read()
        .as_ref()
        .is_some_and(|entry| entry.id == id && entry.state_dir == state_dir)
    {
        return Ok(None);
    }
    let Some(checkpoint) = crate::agent_session_journal::inspect_closed_session_checkpoint(
        id,
        Some(state_dir),
        |_| true,
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(
        cached_transcript(id, &checkpoint.state_dir, &checkpoint.meta)
            .map(ClosedSessionContinuation),
    )
}

pub(crate) async fn remember_flushed_transcript(id: &str) -> Result<(), crate::value::VmError> {
    let store = super::journal_store(id).ok_or_else(|| {
        crate::value::VmError::Runtime("continuation snapshot requires the terminal writer".into())
    })?;
    let meta = store
        .describe(id)
        .await
        .map_err(|error| crate::value::VmError::Runtime(error.to_string()))?;
    let snapshot = SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let state = sessions.get(id)?;
        let source = state.transcript.as_dict()?;
        Some(ClosedTranscript {
            id: id.to_owned(),
            token: uuid::Uuid::now_v7().to_string(),
            state_dir: state.canonical_store_dir.clone()?,
            checkpoint: (&meta).into(),
            transcript: {
                let mut retained = empty_transcript(id)
                    .as_dict()
                    .expect("empty transcript shape")
                    .clone();
                for key in ["messages", "events", "summary"] {
                    if let Some(value) = source.get(key) {
                        retained.put(key, value.clone());
                    }
                }
                VmValue::dict(retained)
            },
            goal: state.goal.clone(),
            text_tool_call_seq: state.text_tool_call_seq,
        })
    });
    *active_session_runtime().closed_transcript.write() = snapshot;
    Ok(())
}

pub(crate) fn cached_transcript(
    id: &str,
    state_dir: &crate::agent_session_journal::CanonicalStoreDir,
    meta: &SessionMeta,
) -> Option<ClosedTranscript> {
    let runtime = active_session_runtime();
    let mut cached = runtime.closed_transcript.write();
    let entry = cached.as_ref()?;
    if entry.id != id || &entry.state_dir != state_dir {
        return None;
    }
    if entry.checkpoint != TranscriptCheckpoint::from(meta) {
        *cached = None;
        return None;
    }
    Some(entry.clone())
}

/// Validate a client-held continuation against the selected canonical store.
/// A missing or changed entry falls back to ordinary replay at the caller.
pub async fn load_closed_session_continuation(
    id: &str,
    token: &str,
) -> Result<Option<ClosedSessionContinuation>, crate::value::VmError> {
    let Some(expected_dir) = active_session_runtime()
        .closed_transcript
        .read()
        .as_ref()
        .filter(|entry| entry.id == id && entry.token == token)
        .map(|entry| entry.state_dir.clone())
    else {
        return Ok(None);
    };
    let Some(checkpoint) =
        crate::agent_session_journal::inspect_closed_session_checkpoint(id, None, |dir| {
            dir == &expected_dir
        })
        .await?
    else {
        return Ok(None);
    };
    if !crate::persistent_state::session_matches_current_workspace(&checkpoint.meta) {
        return Ok(None);
    }
    let Some(cached) = cached_transcript(id, &checkpoint.state_dir, &checkpoint.meta) else {
        return Ok(None);
    };
    Ok(Some(ClosedSessionContinuation(cached)))
}

pub(crate) fn has_continuation_checkpoint(id: &str) -> bool {
    SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .is_some_and(|state| state.continuation_checkpoint.is_some())
    })
}

pub(crate) fn validate_continuation_checkpoint(id: &str, meta: &SessionMeta) -> Result<(), String> {
    SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        if sessions.get(id).is_some_and(|state| {
            state
                .continuation_checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint != &TranscriptCheckpoint::from(meta))
        }) {
            return Err(
                "session history changed after continuation; close and reload the session".into(),
            );
        }
        Ok(())
    })
}

impl ClosedTranscript {
    pub(crate) fn has_messages(&self) -> bool {
        self.transcript
            .as_dict()
            .and_then(|dict| dict.get("messages"))
            .is_some_and(|messages| matches!(messages, VmValue::List(items) if !items.is_empty()))
    }
}
