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
    crate::agent_session_journal::flush(source).await?;
    match store.describe(source).await {
        Ok(_) => {}
        Err(StoreError::NotFound(_)) if super::length(source) == Some(0) => {
            // A newly created, unprompted parent has no context to persist.
            // Name it in the canonical store rather than fabricate history.
            store
                .create(CreateSession {
                    id: Some(source.to_string()),
                    cwd: Some(root.to_string_lossy().into_owned()),
                    project_scope: Some(root.to_string_lossy().into_owned()),
                    parent_session_id: super::parent_id(source),
                    ..CreateSession::default()
                })
                .await
                .map_err(store_error)?;
        }
        Err(error) => return Err(store_error(error)),
    }
    let events = store.read_all(source).await.map_err(store_error)?;
    let hydrated = crate::agent_session_journal::hydrate_events(events.clone());
    // session/load registers a live transport record before the first prompt.
    // Only an active journal owns newer context than the canonical store.
    if !super::has_journal(source) {
        super::replace_messages(source, &hydrated.messages).map_err(VmError::Runtime)?;
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
    if let Err(error) = super::replace_messages(&child, &copied.messages)
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

fn store_error(error: StoreError) -> CanonicalForkError {
    CanonicalForkError::Persistence(VmError::Runtime(format!("canonical session fork: {error}")))
}
