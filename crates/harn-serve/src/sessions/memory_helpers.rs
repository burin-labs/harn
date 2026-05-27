//! Shared helpers used by both the in-memory and sqlite backends.
//!
//! Keeps `meta_for_create` / `validate_open` in one place so backend
//! semantics cannot drift (`create` always seeds the same defaults;
//! `append` always rejects closed and deleted sessions with the same
//! errors).

use uuid::Uuid;

use super::event::now_ms_and_rfc3339;
use super::store::{CreateSession, SessionMeta, SessionStatus, StoreError, StoreResult};

pub(crate) fn meta_for_create(request: CreateSession) -> SessionMeta {
    let (ms, text) = now_ms_and_rfc3339();
    let id = request.id.unwrap_or_else(|| Uuid::now_v7().to_string());
    SessionMeta {
        id,
        tenant_id: request.tenant_id,
        persona: request.persona,
        parent_session_id: request.parent_session_id,
        created_at_ms: ms,
        created_at: text.clone(),
        updated_at_ms: ms,
        updated_at: text,
        status: SessionStatus::Open,
        event_count: 0,
        last_event_id: None,
        chain_root_hash: None,
        closed_at_ms: None,
        closed_at: None,
        soft_deleted_at_ms: None,
        ttl_seconds: request.ttl_seconds,
        tags: request.tags,
        attributes: request.attributes,
    }
}

pub(crate) fn validate_open(meta: &SessionMeta) -> StoreResult<()> {
    match meta.status {
        SessionStatus::Open => Ok(()),
        SessionStatus::Closed => Err(StoreError::Conflict(format!(
            "session '{}' is closed",
            meta.id
        ))),
        SessionStatus::SoftDeleted | SessionStatus::HardDeleted => Err(StoreError::NotFound(
            format!("session '{}' has been deleted", meta.id),
        )),
    }
}
