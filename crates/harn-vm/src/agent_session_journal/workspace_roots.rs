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
    let standalone = if agent_sessions::has_journal(id) {
        super::flush(id).await.map_err(|error| error.to_string())?;
        None
    } else {
        scope
            .run(async {
                let state_dir = session_store::canonical_store_state_dir(None)?;
                super::prepare_standalone(id, Some(state_dir), None, "workspace_root_mount").await
            })
            .await
            .map_err(|error| error.to_string())?
    };
    let event = pending.audit_event(reason);
    let payload = super::SessionEventPayload::Transcript(super::TranscriptRecord::new(
        crate::llm::helpers::vm_value_to_json(&event),
        None,
    ));
    let prepare = |journal: &super::JournalState| {
        super::append_event_for_payload(&journal.config, payload.clone())
            .map(|append| (journal.store(), append))
    };
    let (store, append) = match standalone.as_ref() {
        Some(journal) => prepare(journal),
        None => agent_sessions::with_live_journal(id, |journal| prepare(journal))
            .ok_or_else(|| "workspace root journal disappeared before persistence".to_string())?,
    }
    .map_err(|error| error.to_string())?;
    let directories = pending.directories()?;
    store
        .update(
            id,
            UpdateSession::default().with_additional_directories(directories.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut events = Vec::new();
    if standalone.is_some()
        && !store
            .describe(id)
            .await
            .map_err(|error| error.to_string())?
            .has_canonical_journal()
    {
        // A first audit must not make an empty journal shadow existing live context.
        let prompt = agent_sessions::prompt_state_json(id);
        let transcript = agent_sessions::transcript(id)
            .map(|value| crate::llm::helpers::vm_value_to_json(&value))
            .ok_or_else(|| "workspace root session disappeared before persistence".to_string())?;
        let source_event_ids = transcript
            .get("events")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(|event| super::TranscriptRecord::new(event.clone(), None))
            .filter(|record| record.is_history_position())
            .map(|record| record.transcript_event.id.clone())
            .collect();
        events.push(super::messages_replaced_event(
            prompt.messages,
            prompt.summary,
            source_event_ids,
        ));
    }
    events.push(append);
    store
        .append_batch(id, events)
        .await
        .map_err(|error| error.to_string())?;
    agent_sessions::record_persisted_workspace_directories(id, directories);
    pending.commit_persisted(event)
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
