//! Validate a complete workspace-root replacement before admitting any grant.

use std::path::{Path, PathBuf};

use crate::workspace_anchor::{MountMode, MountedRoot, WorkspaceAnchor};

/// An ordered root set whose directories all exist and can be represented on
/// the host protocol. Preparing it has no session or persistence effects.
pub struct PreparedWorkspaceRoots {
    anchor: WorkspaceAnchor,
}

impl PreparedWorkspaceRoots {
    pub fn prepare(primary: &Path, directories: &[String]) -> Result<Self, String> {
        let primary = validated_absolute_directory(primary)?;
        let mounted_at = crate::orchestration::now_unix_seconds_text();
        let additional_roots = directories
            .iter()
            .map(|directory| {
                Ok(MountedRoot {
                    path: validated_absolute_directory(Path::new(directory))?,
                    mount_mode: MountMode::Extend,
                    mounted_at: mounted_at.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            anchor: WorkspaceAnchor {
                primary,
                additional_roots,
                anchored_at: mounted_at,
            },
        })
    }

    pub fn anchor(&self) -> &WorkspaceAnchor {
        &self.anchor
    }

    /// Persist the accepted path projection before changing live authority.
    /// The canonical writer lease refuses a competing active run.
    pub async fn install(
        mut self,
        session_id: &str,
        scope: &crate::SessionStoreScope,
        session_type: harn_session_store::SessionType,
    ) -> Result<bool, crate::value::VmError> {
        use crate::stdlib::session_store;
        use crate::value::VmError;
        use harn_session_store::{SessionStore, SessionWriteLease, UpdateSession};

        if !super::exists(session_id) {
            return Err(VmError::Runtime(format!("unknown session '{session_id}'")));
        }
        if self.anchor.primary != scope.workspace() {
            return Err(VmError::Runtime(
                "workspace root replacement cannot move the session cwd".into(),
            ));
        }
        if let Some(previous) = super::workspace_anchor(session_id) {
            let previous_primary = previous.primary.canonicalize().map_err(|error| {
                VmError::Runtime(format!("session workspace cannot be resolved: {error}"))
            })?;
            if previous_primary != self.anchor.primary {
                return Err(VmError::Runtime(
                    "workspace root replacement cannot reanchor a session".into(),
                ));
            }
            self.anchor.anchored_at = previous.anchored_at;
            for root in &mut self.anchor.additional_roots {
                if let Some(prior) = previous
                    .additional_roots
                    .iter()
                    .find(|prior| prior.path == root.path && prior.mount_mode == root.mount_mode)
                {
                    root.mounted_at = prior.mounted_at.clone();
                }
            }
        }
        let directories = self
            .anchor
            .additional_directories()
            .map_err(VmError::Runtime)?;
        scope
            .run(async {
                let state_dir = session_store::canonical_store_state_dir(None)?;
                let _lease = SessionWriteLease::try_acquire(state_dir.as_path(), session_id)
                    .map_err(|error| {
                        session_store::session_lease_error("workspace roots", error)
                    })?;
                let store = session_store::open_canonical_agent_session_with_directories(
                    &state_dir,
                    session_id,
                    super::parent_id(session_id),
                    session_type,
                    directories.clone(),
                )
                .await?;
                let metadata = store.describe(session_id).await.map_err(|error| {
                    VmError::Runtime(format!("workspace root metadata: {error}"))
                })?;
                if metadata.additional_directories != directories {
                    store
                        .update(
                            session_id,
                            UpdateSession {
                                additional_directories: Some(directories),
                                ..UpdateSession::default()
                            },
                        )
                        .await
                        .map_err(|error| {
                            VmError::Runtime(format!("workspace root metadata: {error}"))
                        })?;
                }
                super::set_workspace_anchor(session_id, Some(self.anchor)).map_err(VmError::Runtime)
            })
            .await
    }
}

fn validated_absolute_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "workspace root '{}' must be absolute",
            path.display()
        ));
    }
    let path = path
        .to_str()
        .ok_or_else(|| "workspace root must be representable as UTF-8".to_string())?;
    let canonical = super::metadata::validate_workspace_root_path(path)?;
    if canonical.to_str().is_none() {
        return Err("canonical workspace root must be representable as UTF-8".to_string());
    }
    Ok(canonical)
}
