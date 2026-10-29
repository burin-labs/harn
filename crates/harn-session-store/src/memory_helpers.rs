//! Shared helpers used by both the in-memory and sqlite backends.
//!
//! Keeps `meta_for_create` / `validate_open` / `resolve_title_update` in one
//! place so backend semantics cannot drift (`create` always seeds the same
//! defaults; `append` always rejects closed and deleted sessions with the same
//! errors; `update` resolves a title against its pin the same way).

use uuid::Uuid;

use super::event::now_ms_and_rfc3339;
use super::store::{CreateSession, SessionMeta, SessionStatus, StoreError, StoreResult};

/// Decide a session's title and pin state for one update.
///
/// A write that names no pin intent is a *derived* title — generated from
/// session content — and yields to a title a person pinned. Naming an intent
/// claims the title either way, so a rename always wins and an explicit
/// release always lands. Both backends route through here, so the rule has one
/// owner instead of a SQL copy and a Rust copy that can drift apart.
pub(crate) fn resolve_title_update(
    current_title: Option<String>,
    current_pinned: bool,
    requested_title: Option<String>,
    requested_pin: Option<bool>,
) -> (Option<String>, bool) {
    let pinned = requested_pin.unwrap_or(current_pinned);
    let title = match (requested_title, requested_pin) {
        (None, _) => current_title,
        (Some(requested), Some(_)) => Some(requested),
        (Some(requested), None) if !current_pinned => Some(requested),
        (Some(_), None) => current_title,
    };
    (title, pinned)
}

pub(crate) fn meta_for_create(request: CreateSession) -> SessionMeta {
    let (ms, text) = now_ms_and_rfc3339();
    let id = request.id.unwrap_or_else(|| Uuid::now_v7().to_string());
    SessionMeta {
        id,
        tenant_id: request.tenant_id,
        persona: request.persona,
        parent_session_id: request.parent_session_id,
        title: request.title,
        title_pinned: request.title_pinned,
        cwd: request.cwd,
        model: request.model,
        session_type: request.session_type,
        project_scope: request.project_scope,
        usage_input: request.usage_input,
        usage_output: request.usage_output,
        usage_cost_usd_micros: request.usage_cost_usd_micros,
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
