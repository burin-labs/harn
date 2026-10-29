//! Persist workspace projections through the canonical journal owner.

use crate::agent_sessions::{self, PreparedWorkspaceRoots};
use crate::stdlib::session_store;
use crate::value::VmError;
use harn_session_store::{SessionStore, SessionType, SessionWriteLease, UpdateSession};

/// Persist the accepted projection before replacing live workspace authority.
pub async fn install_workspace_roots(
    session_id: &str,
    prepared: PreparedWorkspaceRoots,
    scope: &crate::SessionStoreScope,
    session_type: SessionType,
) -> Result<bool, VmError> {
    let mut anchor = prepared.into_anchor();
    if !agent_sessions::exists(session_id) {
        return Err(VmError::Runtime(format!("unknown session '{session_id}'")));
    }
    if anchor.primary != scope.workspace() {
        return Err(VmError::Runtime(
            "workspace root replacement cannot move the session cwd".into(),
        ));
    }
    if let Some(previous) = agent_sessions::workspace_anchor(session_id) {
        let previous_primary = previous.primary.canonicalize().map_err(|error| {
            VmError::Runtime(format!("session workspace cannot be resolved: {error}"))
        })?;
        if previous_primary != anchor.primary {
            return Err(VmError::Runtime(
                "workspace root replacement cannot reanchor a session".into(),
            ));
        }
        anchor.anchored_at = previous.anchored_at;
        for root in &mut anchor.additional_roots {
            if let Some(prior) = previous
                .additional_roots
                .iter()
                .find(|prior| prior.path == root.path && prior.mount_mode == root.mount_mode)
            {
                root.mounted_at = prior.mounted_at.clone();
            }
        }
    }
    let directories = anchor.additional_directories().map_err(VmError::Runtime)?;
    persist_directory_projection(session_id, scope, session_type, directories).await?;
    agent_sessions::set_workspace_anchor(session_id, Some(anchor)).map_err(VmError::Runtime)
}

pub(crate) async fn add_workspace_root_verified_persisted(
    id: &str,
    root: &str,
    mount_mode: Option<crate::workspace_anchor::MountMode>,
    reason: Option<String>,
    scope: &crate::SessionStoreScope,
    verify: impl FnOnce(&std::path::Path) -> Result<(), String>,
) -> Result<String, String> {
    if agent_sessions::workspace_anchor(id).is_none_or(|anchor| anchor.primary != scope.workspace())
    {
        return Err("workspace root mutation must use the session's captured workspace".into());
    }
    let pending = agent_sessions::prepare_workspace_mount_verified(id, root, mount_mode, verify)?;
    if agent_sessions::has_journal(id) {
        super::flush(id).await.map_err(|error| error.to_string())?;
    } else {
        persist_directory_projection(id, scope, SessionType::User, pending.directories()?)
            .await
            .map_err(|error| error.to_string())?;
    }
    pending.commit(reason)
}

async fn persist_directory_projection(
    session_id: &str,
    scope: &crate::SessionStoreScope,
    session_type: SessionType,
    directories: Vec<String>,
) -> Result<(), VmError> {
    scope
        .run(async {
            let state_dir = session_store::canonical_store_state_dir(None)?;
            let _lease = SessionWriteLease::try_acquire(state_dir.as_path(), session_id)
                .map_err(|error| session_store::session_lease_error("workspace roots", error))?;
            let store = session_store::open_canonical_agent_session_with_directories(
                &state_dir,
                session_id,
                agent_sessions::parent_id(session_id),
                session_type,
                directories.clone(),
            )
            .await?;
            let metadata = store
                .describe(session_id)
                .await
                .map_err(|error| VmError::Runtime(format!("workspace root metadata: {error}")))?;
            if metadata
                .additional_directories()
                .map_err(|error| VmError::Runtime(format!("workspace root metadata: {error}")))?
                != directories
            {
                store
                    .update(
                        session_id,
                        UpdateSession::default().with_additional_directories(directories),
                    )
                    .await
                    .map_err(|error| {
                        VmError::Runtime(format!("workspace root metadata: {error}"))
                    })?;
            }
            Ok(())
        })
        .await
}
