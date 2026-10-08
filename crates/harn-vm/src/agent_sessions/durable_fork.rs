//! Bind a live conversation fork to the existing canonical store operation.

use std::path::Path;

use harn_session_store::{CreateSession, SessionStore, StoreError};

use crate::value::VmError;

#[derive(Debug)]
/// Failure to admit a live child or persist its canonical context.
pub enum CanonicalForkError {
    Admission(super::SessionOpenError),
    Persistence(VmError),
}

#[derive(Clone, Debug)]
pub struct CanonicalForkResult {
    pub session_id: String,
    pub source_boundary: super::CanonicalSessionBoundary,
}

impl From<VmError> for CanonicalForkError {
    fn from(error: VmError) -> Self {
        Self::Persistence(error)
    }
}

impl std::fmt::Display for CanonicalForkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(error) => std::fmt::Display::fmt(error, formatter),
            Self::Persistence(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

/// Restore cold parent context, fork its acknowledged event prefix, and persist
/// the same prefix and lineage before reporting a usable child to a host.
pub async fn fork_canonical(
    store: &dyn SessionStore,
    root: &Path,
    source: &str,
    boundary: Option<super::CanonicalSessionBoundary>,
    destination: Option<String>,
) -> Result<Option<CanonicalForkResult>, CanonicalForkError> {
    if !super::exists(source) {
        return Ok(None);
    }
    ensure_canonical_parent(store, root, source).await?;
    let events = store.read_all(source).await.map_err(store_error)?;
    let hydrated = crate::agent_session_journal::hydrate_events(events.clone());
    // session/load registers a live transport record before the first prompt.
    // Only an active journal owns newer context than the canonical store.
    if !super::has_journal(source) {
        super::replace_messages_with_summary(
            source,
            &hydrated.messages,
            hydrated.summary.as_deref(),
        )
        .map_err(VmError::Runtime)?;
        super::restore_message_event_ids(source, &hydrated.source_event_ids)
            .map_err(VmError::Runtime)?;
    }
    let boundary = boundary.unwrap_or_else(|| {
        events.last().map_or_else(
            || super::CanonicalSessionBoundary::empty(source),
            super::CanonicalSessionBoundary::acknowledged,
        )
    });
    // Refuse before live admission, then revalidate inside the store's atomic
    // fork. A concurrent truncate/rewrite cannot substitute another prefix.
    boundary.validate(source, &events).map_err(store_error)?;
    let event_id = boundary.event_id;
    // Hydrate the historical prefix itself. Later publication, removals, or
    // compaction must not redefine what this acknowledged boundary restores.
    let copied = crate::agent_session_journal::hydrate_events(
        events
            .into_iter()
            .filter(|event| event_id.is_some_and(|tip| event.event_id <= tip))
            .collect(),
    );
    let child = super::fork(source, destination).map_err(CanonicalForkError::Admission)?;
    let Some(child) = child else { return Ok(None) };
    if let Err(error) =
        super::replace_messages_with_summary(&child, &copied.messages, copied.summary.as_deref())
            .and_then(|()| super::restore_message_event_ids(&child, &copied.source_event_ids))
    {
        super::close(&child);
        return Err(VmError::Runtime(error).into());
    }
    if let Err(error) = store
        .fork(source, boundary.clone(), Some(child.clone()))
        .await
    {
        super::close(&child);
        return Err(store_error(error));
    }
    Ok(Some(CanonicalForkResult {
        session_id: child,
        source_boundary: boundary,
    }))
}

/// A boundary lookup and a fork share the same admission for a live, unprompted
/// parent. Nonempty VM-only context must never masquerade as stored history.
pub(super) async fn ensure_canonical_parent(
    store: &dyn SessionStore,
    root: &Path,
    source: &str,
) -> Result<(), VmError> {
    crate::agent_session_journal::flush(source).await?;
    let result = match store.describe(source).await {
        Ok(_) => return Ok(()),
        Err(StoreError::NotFound(_)) if super::length(source) == Some(0) => store
            .create(CreateSession {
                id: Some(source.to_string()),
                cwd: Some(root.to_string_lossy().into_owned()),
                project_scope: Some(root.to_string_lossy().into_owned()),
                parent_session_id: super::parent_id(source),
                ..CreateSession::default()
            })
            .await
            .map(|_| ()),
        Err(error) => Err(error),
    };
    result.map_err(|error| VmError::Runtime(format!("canonical session history: {error}")))
}

fn store_error(error: StoreError) -> CanonicalForkError {
    CanonicalForkError::Persistence(VmError::Runtime(format!("canonical session fork: {error}")))
}
