use super::*;
use harn_vm::agent_sessions::{PreparedWorkspaceRoots, WorkspaceRootsError};

fn same_authority(
    current: &harn_vm::workspace_anchor::WorkspaceAnchor,
    prepared: &PreparedWorkspaceRoots,
) -> bool {
    current.primary == prepared.anchor().primary
        && current
            .additional_roots
            .iter()
            .map(|root| (&root.path, root.mount_mode))
            .eq(prepared
                .anchor()
                .additional_roots
                .iter()
                .map(|root| (&root.path, root.mount_mode)))
}

impl AcpServer {
    pub(super) fn send_root_grant_error(&self, id: &serde_json::Value, error: WorkspaceRootsError) {
        let (code, message) = match error {
            WorkspaceRootsError::Invalid(message) => ("invalid_workspace_roots", message),
            WorkspaceRootsError::OutsideProcess(path) => (
                "outside_process_confinement",
                format!(
                    "workspace root '{}' is outside process confinement",
                    path.display()
                ),
            ),
        };
        self.send_error_with_data(id, -32602, &message, serde_json::json!({"code": code}));
    }

    pub(super) fn prepare_session_roots(
        &self,
        params: &serde_json::Value,
        existing: Option<&str>,
    ) -> Result<PreparedWorkspaceRoots, WorkspaceRootsError> {
        let declared_cwd = match params.get("cwd") {
            None => None,
            Some(serde_json::Value::String(cwd)) if !cwd.is_empty() => Some(cwd.as_str()),
            Some(_) => return Err("cwd must be a nonempty directory path".into()),
        };
        let live = existing.and_then(|id| self.sessions.get(id));
        let cwd = match (declared_cwd, live) {
            (Some(cwd), _) => std::path::absolute(cwd).map_err(|error| error.to_string())?,
            (None, Some(session)) => session.cwd.clone(),
            (None, None) if existing.is_some() => self
                .restore_store_scope(params)
                .map_err(|error| error.to_string())?
                .workspace()
                .to_path_buf(),
            (None, None) => std::env::current_dir().map_err(|error| error.to_string())?,
        };
        let directories: Vec<String> = match params.get("additionalDirectories") {
            None => Vec::new(),
            Some(value) => serde_json::from_value(value.clone()).map_err(|_| {
                "additionalDirectories must be an array of directory paths".to_string()
            })?,
        };
        let prepared = PreparedWorkspaceRoots::prepare(&cwd, &directories)?;
        if let Some(session) = live {
            let live_cwd = session
                .cwd
                .canonicalize()
                .map_err(|error| error.to_string())?;
            if prepared.anchor().primary != live_cwd {
                return Err("session cwd cannot change during load or resume".into());
            }
        }
        if let (Some(id), Some(session)) = (existing, live) {
            let current = harn_vm::agent_sessions::workspace_anchor(id);
            let unchanged = current
                .as_ref()
                .is_some_and(|anchor| same_authority(anchor, &prepared));
            if !unchanged && session.concurrent_control.prompt_is_active() {
                return Err("workspace roots cannot change during an active prompt".into());
            }
        }
        Ok(prepared)
    }

    pub(super) async fn install_session_roots(
        &self,
        session_id: &str,
        prepared: PreparedWorkspaceRoots,
    ) -> Result<(), String> {
        let session = self.sessions.get(session_id).ok_or("unknown session")?;
        let current = harn_vm::agent_sessions::workspace_anchor(session_id);
        if session.concurrent_control.prompt_is_active()
            && current
                .as_ref()
                .is_some_and(|anchor| same_authority(anchor, &prepared))
        {
            return Ok(());
        }
        Box::pin(harn_vm::install_session_workspace_roots(
            session_id,
            prepared,
            &session.store_scope,
            harn_session_store::SessionType::User,
        ))
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    pub(super) fn rollback_session_admission(&mut self, session_id: &str) {
        harn_vm::agent_sessions::close(session_id);
        self.release_session_resources(session_id);
        self.known_sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
    }
}
