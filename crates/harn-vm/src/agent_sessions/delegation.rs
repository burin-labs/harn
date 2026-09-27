//! Opening a session as the delegated child of another (harn#8927).

use super::{exists, open_child_session_with_actor, open_or_create, SessionOpenError};

/// Open `id`, as a child of `parent` when one is named. `actor` names the
/// delegated child and is pushed onto the parent's actor chain, so it needs a
/// `parent` that already exists; without either this is [`open_or_create`].
pub fn open_session(
    id: Option<String>,
    parent: Option<&str>,
    actor: Option<&str>,
) -> Result<String, SessionOpenError> {
    let rejected = |session_id: &str, reason: &str| SessionOpenError::LineageRejected {
        session_id: session_id.to_string(),
        reason: reason.to_string(),
    };
    match parent {
        Some(parent) if exists(parent) => open_child_session_with_actor(parent, id, actor),
        Some(parent) => Err(rejected(parent, "unknown parent session")),
        None if actor.is_some() => Err(rejected(
            id.as_deref().unwrap_or_default(),
            "`actor` names a delegated child and requires `parent`",
        )),
        None => open_or_create(id),
    }
}
