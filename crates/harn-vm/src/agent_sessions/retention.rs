use super::{ActorChain, SessionOpenError};
use std::sync::Arc;

/// Retains this exact session generation against automatic eviction.
/// Explicit close remains authoritative, even while a host holds this handle.
#[derive(Clone, Debug)]
pub struct SessionRetention {
    _generation: Arc<()>,
}

impl SessionRetention {
    pub(super) fn for_state(state: &super::SessionState) -> Self {
        Self {
            _generation: state.retention.clone(),
        }
    }

    pub(super) fn is_current(&self, id: &str) -> bool {
        super::SESSIONS.with(|sessions| {
            sessions
                .borrow()
                .get(id)
                .is_some_and(|state| Arc::ptr_eq(&state.retention, &self._generation))
        })
    }
}

pub fn open_or_create_with_actor_chain(
    id: Option<String>,
    requested_actor_chain: Option<ActorChain>,
) -> Result<String, SessionOpenError> {
    super::open_with_retention(id, requested_actor_chain, false).map(|(id, _)| id)
}

pub fn open_retained_with_actor_chain(
    id: Option<String>,
    requested_actor_chain: Option<ActorChain>,
) -> Result<(String, SessionRetention), SessionOpenError> {
    let (id, retention) = super::open_with_retention(id, requested_actor_chain, true)?;
    Ok((
        id,
        retention.expect("retained admission returns its generation handle"),
    ))
}

/// Fork a transient conversation without retaining it for a host.
pub fn fork(src_id: &str, dst_id: Option<String>) -> Result<Option<String>, SessionOpenError> {
    super::fork_with_retention(src_id, dst_id, false).map(|fork| fork.map(|(id, _)| id))
}

pub(super) fn fork_retained(
    src_id: &str,
    dst_id: Option<String>,
) -> Result<Option<(String, SessionRetention)>, SessionOpenError> {
    super::fork_with_retention(src_id, dst_id, true).map(|fork| {
        fork.map(|(id, retention)| {
            (
                id,
                retention.expect("retained fork returns its generation handle"),
            )
        })
    })
}
