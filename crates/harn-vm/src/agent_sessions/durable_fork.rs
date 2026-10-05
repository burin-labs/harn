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

/// Restore cold parent context, fork its selected message prefix, and persist
/// the same prefix and lineage before reporting a usable child to a host.
pub async fn fork_canonical(
    root: &Path,
    source: &str,
    keep_first: Option<usize>,
    destination: Option<String>,
) -> Result<Option<String>, CanonicalForkError> {
    if !super::exists(source) {
        return Ok(None);
    }
    crate::agent_session_journal::flush(source).await?;
    let store = crate::stdlib::session_store::open_canonical_store(root)?;
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
    let events = crate::stdlib::session_store::read_all_events(&store, source).await?;
    let hydrated = crate::agent_session_journal::hydrate_events(events.clone());
    // session/load registers a live transport record before the first prompt.
    // Only an active journal owns newer context than the canonical store.
    if !super::has_journal(source) {
        super::replace_messages(source, &hydrated.messages).map_err(VmError::Runtime)?;
        super::restore_message_event_ids(source, &hydrated.source_event_ids)
            .map_err(VmError::Runtime)?;
    }
    let boundary = match keep_first {
        Some(0) => None,
        Some(count) if count < hydrated.messages.len() => {
            let identity = hydrated.source_event_ids[count - 1]
                .as_deref()
                .ok_or_else(|| {
                    VmError::Runtime("canonical fork prefix has no source event identity".into())
                })?;
            Some(
                events
                    .iter()
                    .find(|event| {
                        event
                            .headers
                            .get("source_event_id")
                            .is_some_and(|id| id == identity)
                    })
                    .ok_or_else(|| {
                        VmError::Runtime("canonical fork prefix identity is not persisted".into())
                    })?
                    .event_id,
            )
        }
        _ => events.last().map(|event| event.event_id),
    };
    // Compaction, removals, and publication can change a message without
    // changing its original identity. Never persist an earlier event prefix
    // that would hydrate into different context after restarting the child.
    let copied = crate::agent_session_journal::hydrate_events(
        events
            .into_iter()
            .filter(|event| boundary.is_some_and(|tip| event.event_id <= tip))
            .collect(),
    );
    let count = keep_first
        .unwrap_or(hydrated.messages.len())
        .min(hydrated.messages.len());
    if copied.messages != hydrated.messages[..count] {
        return Err(VmError::Runtime(
            "selected message prefix has no canonical event boundary after transcript replacement"
                .into(),
        )
        .into());
    }
    let child = match keep_first {
        Some(count) => super::fork_at(source, count, destination),
        None => super::fork(source, destination),
    }
    .map_err(CanonicalForkError::Admission)?;
    let Some(child) = child else { return Ok(None) };
    if let Err(error) = store.fork(source, boundary, Some(child.clone())).await {
        super::close(&child);
        return Err(store_error(error));
    }
    Ok(Some(child))
}

fn store_error(error: StoreError) -> CanonicalForkError {
    CanonicalForkError::Persistence(VmError::Runtime(format!("canonical session fork: {error}")))
}
