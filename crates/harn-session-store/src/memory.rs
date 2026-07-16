//! In-memory backend. Single-process, no persistence — but matches
//! the public [`SessionStore`] contract exactly so the rest of the
//! primitive can be exercised end-to-end in tests without touching
//! disk.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use uuid::Uuid;

use super::event::{now_ms_and_rfc3339, AppendEvent, EventId, SessionEventKind, StoredEvent};
use super::memory_helpers::{meta_for_create, validate_open};
use super::redaction::{prepare_append_event, redact_stored_events};
use super::signing::{
    chain_root_fold, chain_root_hash, chain_root_init, compute_record_hash, re_anchor_events,
    verify_event_chain,
};
use super::store::{
    CreateSession, EventPage, ForkResult, ListFilter, ReadRange, SessionId, SessionMeta,
    SessionStatus, SessionStore, Snapshot, SnapshotId, StoreError, StoreHooks, StoreResult,
    TruncateResult, VerifyFailure, VerifyReport, MAX_READ_BATCH,
};

struct SessionRecord {
    meta: SessionMeta,
    events: Vec<StoredEvent>,
    next_event_id: EventId,
}

impl SessionRecord {
    fn fresh(meta: SessionMeta) -> Self {
        Self {
            meta,
            events: Vec::new(),
            next_event_id: 1,
        }
    }
}

#[derive(Default)]
struct Inner {
    sessions: BTreeMap<SessionId, SessionRecord>,
    snapshots: BTreeMap<String, Snapshot>,
}

#[derive(Clone)]
pub struct MemorySessionStore {
    inner: Arc<Mutex<Inner>>,
    hooks: Arc<StoreHooks>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::with_hooks(StoreHooks::default())
    }

    pub fn with_hooks(hooks: StoreHooks) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            hooks: Arc::new(hooks),
        }
    }
}

impl Default for MemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn lock(inner: &Arc<Mutex<Inner>>) -> std::sync::MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

/// Core append logic against an already-locked session record. Both
/// `append` and `close` call this while holding the single store lock, so
/// `close` can read the chain state, append the receipt, and finalise it
/// without releasing the guard in between (which previously let a
/// concurrent append interleave and displace the receipt).
fn append_locked(
    record: &mut SessionRecord,
    hooks: &StoreHooks,
    mut event: AppendEvent,
) -> StoreResult<StoredEvent> {
    prepare_append_event(hooks, &mut event)?;
    validate_open(&record.meta)?;
    validate_parent(record, &event)?;
    let (ts_ms, ts) = now_ms_and_rfc3339();
    let event_id = record.next_event_id;
    record.next_event_id = record.next_event_id.saturating_add(1);
    let prev_hash = record.events.last().map(|tail| tail.record_hash.clone());
    let mut stored = StoredEvent {
        event_id,
        session_id: record.meta.id.clone(),
        tenant_id: record.meta.tenant_id.clone(),
        parent_event_id: event.parent_event_id,
        actor: event.actor,
        kind: event.kind,
        payload: event.payload,
        tags: event.tags,
        headers: event.headers,
        ts_ms,
        ts,
        record_hash: String::new(),
        prev_hash,
        signed_by: None,
    };
    stored.record_hash = compute_record_hash(&stored);
    if let Some(signer) = hooks.event_signer.as_ref() {
        stored.signed_by = Some(signer.sign_event(&stored));
    }
    let prev_root = record
        .meta
        .chain_root_hash
        .clone()
        .unwrap_or_else(chain_root_init);
    record.events.push(stored.clone());
    record.meta.event_count = record.events.len();
    record.meta.last_event_id = Some(event_id);
    record.meta.updated_at_ms = ts_ms;
    record.meta.updated_at = stored.ts.clone();
    record.meta.chain_root_hash = Some(chain_root_fold(&prev_root, &stored.record_hash));
    Ok(stored)
}

#[async_trait]
impl SessionStore for MemorySessionStore {
    fn hooks(&self) -> &StoreHooks {
        &self.hooks
    }

    async fn create(&self, request: CreateSession) -> StoreResult<SessionMeta> {
        let meta = meta_for_create(request);
        let mut guard = lock(&self.inner);
        if guard.sessions.contains_key(&meta.id) {
            return Err(StoreError::AlreadyExists(meta.id));
        }
        guard
            .sessions
            .insert(meta.id.clone(), SessionRecord::fresh(meta.clone()));
        Ok(meta)
    }

    async fn describe(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let guard = lock(&self.inner);
        guard
            .sessions
            .get(session_id)
            .map(|record| record.meta.clone())
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
    }

    async fn list(&self, filter: ListFilter) -> StoreResult<Vec<SessionMeta>> {
        let guard = lock(&self.inner);
        let limit = filter.limit.unwrap_or(MAX_READ_BATCH).min(MAX_READ_BATCH);
        let mut out: Vec<SessionMeta> = guard
            .sessions
            .values()
            .map(|record| record.meta.clone())
            .filter(|meta| match_filter(meta, &filter))
            .collect();
        out.sort_by_key(|meta| meta.created_at_ms);
        if let Some(cursor) = filter.cursor.as_ref() {
            let position = out.iter().position(|meta| meta.id == *cursor);
            if let Some(start) = position {
                out = out.into_iter().skip(start + 1).collect();
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    async fn append(&self, session_id: &str, event: AppendEvent) -> StoreResult<StoredEvent> {
        let mut guard = lock(&self.inner);
        let record = guard
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        append_locked(record, &self.hooks, event)
    }

    async fn read(&self, session_id: &str, range: ReadRange) -> StoreResult<EventPage> {
        let guard = lock(&self.inner);
        let record = guard
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let from = range.from_event_id.unwrap_or(1);
        let to = range.to_event_id.unwrap_or(EventId::MAX);
        let limit = range.limit.unwrap_or(MAX_READ_BATCH).min(MAX_READ_BATCH);
        let mut events: Vec<StoredEvent> = record
            .events
            .iter()
            .filter(|event| event.event_id >= from && event.event_id <= to)
            .take(limit)
            .cloned()
            .collect();
        let next_cursor = if events.len() == limit {
            events.last().map(|tail| tail.event_id + 1)
        } else {
            None
        };
        // Defense in depth for data imported or written under an older policy.
        redact_stored_events(&self.hooks, &mut events)?;
        Ok(EventPage {
            events,
            next_cursor,
        })
    }

    async fn fork(
        &self,
        session_id: &str,
        at_event_id: EventId,
        child_id: Option<SessionId>,
    ) -> StoreResult<ForkResult> {
        let mut guard = lock(&self.inner);
        let parent = guard
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        if !parent
            .events
            .iter()
            .any(|event| event.event_id == at_event_id)
        {
            return Err(StoreError::InvalidInput(format!(
                "event {at_event_id} not found in session '{session_id}'"
            )));
        }
        let new_id = child_id.unwrap_or_else(|| Uuid::now_v7().to_string());
        if guard.sessions.contains_key(&new_id) {
            return Err(StoreError::AlreadyExists(new_id));
        }
        let parent = guard.sessions.get(session_id).unwrap();
        let (ms, text) = now_ms_and_rfc3339();
        let mut child_meta = parent.meta.clone();
        child_meta.id = new_id.clone();
        child_meta.parent_session_id = Some(parent.meta.id.clone());
        child_meta.created_at_ms = ms;
        child_meta.created_at = text.clone();
        child_meta.updated_at_ms = ms;
        child_meta.updated_at = text;
        child_meta.status = SessionStatus::Open;
        child_meta.closed_at = None;
        child_meta.closed_at_ms = None;
        child_meta.soft_deleted_at_ms = None;
        let parent_events: Vec<StoredEvent> = parent
            .events
            .iter()
            .filter(|event| event.event_id <= at_event_id)
            .cloned()
            .collect();
        let copied_events = re_anchor_events(&parent_events, &new_id);
        let copied_event_count = copied_events.len();
        child_meta.event_count = copied_event_count;
        child_meta.last_event_id = copied_events.last().map(|tail| tail.event_id);
        child_meta.chain_root_hash = Some(chain_root_hash(&copied_events));
        let next_event_id = copied_events
            .last()
            .map(|tail| tail.event_id + 1)
            .unwrap_or(1);
        let child_record = SessionRecord {
            meta: child_meta,
            events: copied_events,
            next_event_id,
        };
        guard.sessions.insert(new_id.clone(), child_record);
        Ok(ForkResult {
            child_session_id: new_id,
            forked_from_event_id: at_event_id,
            copied_event_count,
        })
    }

    async fn truncate(
        &self,
        session_id: &str,
        at_event_id: EventId,
    ) -> StoreResult<TruncateResult> {
        let mut guard = lock(&self.inner);
        let record = guard
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        if !record
            .events
            .iter()
            .any(|event| event.event_id == at_event_id)
        {
            return Err(StoreError::InvalidInput(format!(
                "event {at_event_id} not found in session '{session_id}'"
            )));
        }
        let removed = record
            .events
            .iter()
            .filter(|event| event.event_id > at_event_id)
            .count();
        record.events.retain(|event| event.event_id <= at_event_id);
        record.next_event_id = at_event_id + 1;
        record.meta.event_count = record.events.len();
        record.meta.last_event_id = record.events.last().map(|tail| tail.event_id);
        record.meta.chain_root_hash = Some(chain_root_hash(&record.events));
        let (ms, text) = now_ms_and_rfc3339();
        record.meta.updated_at_ms = ms;
        record.meta.updated_at = text;
        Ok(TruncateResult {
            kept_event_count: record.events.len(),
            removed_event_count: removed,
            new_tip_event_id: record.events.last().map(|tail| tail.event_id),
        })
    }

    async fn snapshot(&self, session_id: &str) -> StoreResult<Snapshot> {
        let mut guard = lock(&self.inner);
        let record = guard
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let (ms, text) = now_ms_and_rfc3339();
        let snapshot = Snapshot {
            id: SnapshotId(format!("snap-{}", Uuid::now_v7())),
            session: record.meta.clone(),
            events: record.events.clone(),
            captured_at_ms: ms,
            captured_at: text,
        };
        guard
            .snapshots
            .insert(snapshot.id.0.clone(), snapshot.clone());
        Ok(snapshot)
    }

    async fn replay(&self, snapshot_id: &SnapshotId) -> StoreResult<Snapshot> {
        let guard = lock(&self.inner);
        guard
            .snapshots
            .get(&snapshot_id.0)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(snapshot_id.0.clone()))
    }

    async fn close(&self, session_id: &str) -> StoreResult<StoredEvent> {
        // Hold the single store lock across read -> append receipt ->
        // finalise so no concurrent append can interleave and move the
        // tip off the receipt we just minted.
        let mut guard = lock(&self.inner);
        let record = guard
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        validate_open(&record.meta)?;
        let record_root = record
            .meta
            .chain_root_hash
            .clone()
            .unwrap_or_else(|| chain_root_hash(&record.events));
        let last_event_id = record.meta.last_event_id.unwrap_or(0);
        let payload =
            super::signing::canonical_receipt_payload(session_id, last_event_id, &record_root);
        let mut append = AppendEvent::new(SessionEventKind::Receipt, payload);
        append.actor = Some("session_store".into());
        let mut stored = append_locked(record, &self.hooks, append)?;
        // Intentionally replace the receipt's append-time per-event
        // signature with a receipt-root signature: the receipt attests the
        // chain root, so `verify()` checks it via `verify_receipt_root`
        // against the pre-receipt root, not the event's own bytes.
        if let Some(signer) = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
        {
            let signature = signer.sign_receipt(&record_root);
            // Locate the receipt by its event id rather than `last_mut()`,
            // so we can never sign a different event.
            if let Some(receipt) = record
                .events
                .iter_mut()
                .find(|event| event.event_id == stored.event_id)
            {
                receipt.signed_by = Some(signature.clone());
            }
            stored.signed_by = Some(signature);
        }
        let (ms, text) = now_ms_and_rfc3339();
        record.meta.status = SessionStatus::Closed;
        record.meta.closed_at_ms = Some(ms);
        record.meta.closed_at = Some(text);
        Ok(stored)
    }

    async fn soft_delete(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let mut guard = lock(&self.inner);
        let record = guard
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        match record.meta.status {
            SessionStatus::HardDeleted => return Err(StoreError::NotFound(session_id.to_string())),
            SessionStatus::SoftDeleted => return Ok(record.meta.clone()),
            _ => {}
        }
        let (ms, text) = now_ms_and_rfc3339();
        record.meta.status = SessionStatus::SoftDeleted;
        record.meta.soft_deleted_at_ms = Some(ms);
        record.meta.updated_at_ms = ms;
        record.meta.updated_at = text;
        Ok(record.meta.clone())
    }

    async fn hard_delete(&self, session_id: &str) -> StoreResult<()> {
        let mut guard = lock(&self.inner);
        guard
            .sessions
            .remove(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        Ok(())
    }

    async fn verify(&self, session_id: &str) -> StoreResult<VerifyReport> {
        let guard = lock(&self.inner);
        let record = guard
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
        let chain_root = chain_root_hash(&record.events);
        let event_verifier = self
            .hooks
            .event_signer
            .as_ref()
            .map(|signer| signer.verifying_key());
        let receipt_verifier = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
            .map(|signer| signer.verifying_key());
        let (signed, failures) = verify_event_chain(
            &record.events,
            event_verifier.as_ref(),
            receipt_verifier.as_ref(),
        );
        Ok(VerifyReport {
            session_id: session_id.to_string(),
            chain_root_hash: chain_root,
            event_count: record.events.len(),
            signed_event_count: signed,
            failures: failures
                .into_iter()
                .map(|(event_id, reason)| VerifyFailure { event_id, reason })
                .collect(),
        })
    }
}

fn validate_parent(record: &SessionRecord, event: &AppendEvent) -> StoreResult<()> {
    let Some(parent_event_id) = event.parent_event_id else {
        return Ok(());
    };
    if record
        .events
        .iter()
        .any(|stored| stored.event_id == parent_event_id)
    {
        Ok(())
    } else {
        Err(StoreError::InvalidInput(format!(
            "parent_event_id {parent_event_id} not present in session"
        )))
    }
}

fn match_filter(meta: &SessionMeta, filter: &ListFilter) -> bool {
    if let Some(tenant) = filter.tenant_id.as_ref() {
        if meta.tenant_id.as_deref() != Some(tenant.as_str()) {
            return false;
        }
    }
    if let Some(persona) = filter.persona.as_ref() {
        if meta.persona.as_deref() != Some(persona.as_str()) {
            return false;
        }
    }
    if let Some(status) = filter.status {
        if meta.status != status {
            return false;
        }
    }
    if let Some(tag) = filter.tag.as_ref() {
        if !meta.tags.iter().any(|value| value == tag) {
            return false;
        }
    }
    if let Some(after) = filter.created_after_ms {
        if meta.created_at_ms < after {
            return false;
        }
    }
    if let Some(before) = filter.created_before_ms {
        if meta.created_at_ms > before {
            return false;
        }
    }
    true
}
