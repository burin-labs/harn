use super::*;

impl AcpServer {
    pub(super) fn handle_session_new(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        // Resolve the declared capability profile (if any) at the launch
        // boundary, snapshotting the server environment for env-source grants.
        // A malformed profile config or a rejected launch (e.g. a grant on a
        // hermetic profile) fails the session loudly rather than silently
        // downgrading to an ungoverned environment.
        let capability_profile = match self.resolve_session_profile(params) {
            Ok(profile) => profile,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };

        let session_id = self.next_session_id();
        self.insert_session(session_id.clone(), cwd, SessionInfo::default());
        if capability_profile.is_some() {
            if let Some(session) = self.sessions.get_mut(&session_id) {
                session.capability_profile = capability_profile;
            }
        }
        let session = self
            .session_item_json(&session_id, "live", None)
            .unwrap_or_else(|| serde_json::json!({"sessionId": session_id}));

        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "session": session,
                "modes": modes::session_mode_state(modes::DEFAULT_MODE_ID),
                "configOptions": self.config_options_for_session(&session_id, modes::DEFAULT_MODE_ID),
            }),
        );

        self.emit_available_commands(&session_id);
    }

    /// Parse and launch the `profile` block of a `session/new` request, if
    /// present. Returns `Ok(None)` when no profile was declared (the legacy
    /// path), `Ok(Some(profile))` for a resolved profile, and `Err(message)`
    /// for a malformed config or a rejected launch. Env-source grants are
    /// snapshotted from the server environment here, at the launch boundary.
    fn resolve_session_profile(
        &self,
        params: &serde_json::Value,
    ) -> Result<Option<harn_vm::security::SessionProfile>, String> {
        let Some(raw) = params.get("profile") else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        let config: AcpSessionProfileConfig = serde_json::from_value(raw.clone())
            .map_err(|error| format!("invalid session profile config: {error}"))?;
        let profile =
            harn_vm::security::SessionProfile::launch(config.kind, config.grants, &|name| {
                std::env::var(name).ok()
            })
            .map_err(|error| format!("session profile launch failed: {error}"))?;
        Ok(Some(profile))
    }

    pub(super) fn ensure_workspace_anchor(
        &self,
        session_id: &str,
    ) -> Result<harn_vm::workspace_anchor::WorkspaceAnchor, String> {
        if let Some(anchor) = harn_vm::agent_sessions::workspace_anchor(session_id) {
            return Ok(anchor);
        }
        let Some(session) = self.sessions.get(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        let anchor = harn_vm::workspace_anchor::WorkspaceAnchor {
            primary: session.cwd.clone(),
            additional_roots: Vec::new(),
            anchored_at: now_rfc3339(),
        };
        harn_vm::agent_sessions::set_workspace_anchor(session_id, Some(anchor.clone()))?;
        Ok(anchor)
    }

    pub(super) fn handle_harn_session_workspace_roots(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        let anchor = match self.ensure_workspace_anchor(&session_id) {
            Ok(anchor) => anchor,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "workspaceAnchor": anchor.to_json(),
            }),
        );
    }

    pub(super) fn handle_harn_session_add_root(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        if let Err(message) = self.ensure_workspace_anchor(&session_id) {
            self.send_error(id, -32602, &message);
            return;
        }
        let Some(path) =
            string_param(params, "path", "path").or_else(|| string_param(params, "root", "root"))
        else {
            self.send_error(id, -32602, "Missing path");
            return;
        };
        let mount_mode = match mount_mode_param(params) {
            Ok(mount_mode) => mount_mode,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        let reason = string_param(params, "reason", "reason");
        let mounted_at = match harn_vm::agent_sessions::add_workspace_root(
            &session_id,
            &path,
            mount_mode,
            reason,
        ) {
            Ok(mounted_at) => mounted_at,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        let workspace_anchor = harn_vm::agent_sessions::workspace_anchor(&session_id)
            .map(|anchor| anchor.to_json())
            .unwrap_or(serde_json::Value::Null);
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "mountedAt": mounted_at,
                "workspaceAnchor": workspace_anchor,
            }),
        );
    }

    pub(super) fn handle_harn_session_reanchor(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        if let Err(message) = self.ensure_workspace_anchor(&session_id) {
            self.send_error(id, -32602, &message);
            return;
        }
        let Some(path) = string_param(params, "path", "path")
            .or_else(|| string_param(params, "primary", "primary"))
        else {
            self.send_error(id, -32602, "Missing path");
            return;
        };
        let compact = params
            .get("compact")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if compact {
            self.send_error(
                id,
                -32602,
                "harn.session_reanchor does not support compact yet",
            );
            return;
        }
        let carry_transcript = params
            .get("carryTranscript")
            .or_else(|| params.get("carry_transcript"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        if !carry_transcript {
            self.send_error(
                id,
                -32602,
                "harn.session_reanchor does not support carryTranscript=false yet",
            );
            return;
        }
        let reason = string_param(params, "reason", "reason");
        let next_project_root = session_project_root_for_cwd(&PathBuf::from(&path));
        if let Err(message) = self.ensure_session_root_can_move(&session_id, &next_project_root) {
            self.send_error(id, -32602, &message);
            return;
        }
        let anchor = harn_vm::workspace_anchor::WorkspaceAnchor {
            primary: PathBuf::from(path),
            additional_roots: Vec::new(),
            anchored_at: now_rfc3339(),
        };
        let outcome = match harn_vm::agent_sessions::reanchor_session(
            &session_id,
            anchor,
            carry_transcript,
            false,
            reason,
        ) {
            Ok(outcome) => outcome,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        if let Err(message) = self.sync_session_root_from_workspace_anchor(&session_id) {
            self.send_error(id, -32602, &message);
            return;
        }
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "changed": outcome.changed,
                "previousWorkspaceAnchor": outcome.previous.map(|anchor| anchor.to_json()),
                "workspaceAnchor": outcome.current.to_json(),
            }),
        );
    }

    pub(super) fn ensure_session_root_can_move(
        &self,
        session_id: &str,
        next_project_root: &Path,
    ) -> Result<(), String> {
        let Some(session) = self.sessions.get(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        if session.project_root.as_path() == next_project_root {
            return Ok(());
        }
        #[cfg(feature = "hostlib")]
        {
            let status = harn_hostlib::fs::staged_status(session_id).map_err(|error| {
                format!("failed to inspect staged filesystem state for {session_id}: {error}")
            })?;
            if !status.pending_writes.is_empty() {
                return Err(format!(
                    "cannot change session project root with {} staged filesystem change(s) pending; commit or discard staged changes first",
                    status.pending_writes.len()
                ));
            }
        }
        Ok(())
    }

    pub(super) fn sync_session_root_from_workspace_anchor(
        &mut self,
        session_id: &str,
    ) -> Result<(), String> {
        let Some(anchor) = harn_vm::agent_sessions::workspace_anchor(session_id) else {
            return Ok(());
        };
        let next_cwd = anchor.primary;
        let next_project_root = session_project_root_for_cwd(&next_cwd);
        let needs_update = match self.sessions.get(session_id) {
            Some(session) => session.cwd != next_cwd || session.project_root != next_project_root,
            None => return Err(format!("Unknown session: {session_id}")),
        };
        if !needs_update {
            return Ok(());
        }
        self.ensure_session_root_can_move(session_id, &next_project_root)?;
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        session.cwd = next_cwd;
        session.project_root = next_project_root;
        #[cfg(feature = "hostlib")]
        harn_hostlib::fs::configure_session_root(session_id, &session.project_root);
        Ok(())
    }

    /// Read the configured pipeline source for `session_id`. Returns
    /// `None` for inline-prompt sessions (no `--pipeline`) and on read
    /// error — the regular prompt path will surface the error to the
    /// client at execution time.
    pub(super) fn read_pipeline_source(&self, session_id: &str) -> Option<String> {
        let pipeline_path = self.pipeline.as_deref()?;
        let cwd = &self.sessions.get(session_id)?.cwd;
        let full_path = if std::path::Path::new(pipeline_path).is_absolute() {
            PathBuf::from(pipeline_path)
        } else {
            cwd.join(pipeline_path)
        };
        std::fs::read_to_string(&full_path).ok()
    }

    /// Discover and emit `available_commands_update` if the command set
    /// has changed since the last emission for this session.
    pub(super) fn emit_available_commands(&mut self, session_id: &str) {
        let Some(source) = self.read_pipeline_source(session_id) else {
            return;
        };
        self.refresh_advertised_commands(session_id, &source);
    }

    /// Hot-reload variant of [`Self::emit_available_commands`] that uses
    /// pre-loaded source instead of re-reading from disk. Driven from
    /// `handle_session_prompt` on every prompt so editor changes between
    /// prompts propagate to the client without a restart.
    pub(super) fn refresh_advertised_commands(&mut self, session_id: &str, source: &str) {
        let commands = discover_commands(source);
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        if session.advertised_commands == commands {
            return;
        }
        session.advertised_commands = commands.clone();
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": render_available_commands(&commands),
                },
            }),
        );
    }

    pub(super) fn emit_session_info_update(&self, session_id: &str, info: &SessionInfo) {
        let mut update = serde_json::json!({
            "sessionUpdate": "session_info_update",
        });
        if let Some(title) = &info.title {
            update["title"] = serde_json::json!(title);
        }
        if !info.meta.is_empty() {
            update["_meta"] = serde_json::Value::Object(info.meta.clone());
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
    }

    pub(super) fn begin_profile_turn(&mut self, session_id: &str) -> u64 {
        if !self.profile.is_enabled() {
            return 0;
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return 0;
        };
        session.profile_turn += 1;
        harn_vm::tracing::set_tracing_enabled(true);
        session.profile_turn
    }

    pub(super) fn finish_profile_turn(&self, session_id: &str, turn: u64) {
        if turn == 0 || !self.profile.is_enabled() {
            return;
        }
        let spans = harn_vm::tracing::take_spans();
        let rollup = harn_vm::profile::build(&spans);
        if self.profile.text {
            eprintln!("[harn] ACP profile session={session_id} turn={turn}");
            eprint!("{}", harn_vm::profile::render(&rollup));
        }
        if let Some(path) = self.profile.json_path.as_ref() {
            if let Err(error) = append_profile_json_line(path, session_id, turn, &rollup) {
                eprintln!("warning: failed to write ACP profile: {error}");
            }
        }
    }

    pub(super) fn handle_session_fork(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let src_id = session_id_param(params);
        let Some(src_id) = src_id else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        let Some(src_cwd) = self
            .sessions
            .get(&src_id)
            .map(|session| session.cwd.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {src_id}"));
            return;
        };

        if !harn_vm::agent_sessions::exists(&src_id) {
            harn_vm::agent_sessions::open_or_create(Some(src_id.clone()));
        }

        let keep_first =
            match nonnegative_usize_param(params, &["keep_first", "keepFirst"], "keep_first") {
                Ok(value) => value,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    return;
                }
            };
        let dst_id = params
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if let Some(dst_id) = dst_id.as_deref() {
            if self.sessions.contains_key(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
            if harn_vm::agent_sessions::exists(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
        }
        let branch_name = params
            .get("branch_name")
            .and_then(|value| value.as_str())
            .map(str::to_string);

        let new_session_id = match keep_first {
            Some(keep_first) => harn_vm::agent_sessions::fork_at(&src_id, keep_first, dst_id),
            None => harn_vm::agent_sessions::fork(&src_id, dst_id),
        };
        let Some(new_session_id) = new_session_id else {
            self.send_error(id, -32000, &format!("Failed to fork session: {src_id}"));
            return;
        };

        let snapshot = harn_vm::agent_sessions::snapshot(&new_session_id)
            .and_then(|value| serde_json::to_value(harn_vm::llm::vm_value_to_json(&value)).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let branched_at = snapshot
            .get("branched_at_event_index")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let mut meta = serde_json::Map::new();
        meta.insert("state".to_string(), serde_json::json!("forked"));
        meta.insert("parent_id".to_string(), serde_json::json!(src_id));
        meta.insert("branched_at".to_string(), branched_at.clone());
        if let Some(branch_name) = &branch_name {
            meta.insert("branch_name".to_string(), serde_json::json!(branch_name));
        }
        let info = SessionInfo {
            title: branch_name,
            meta,
        };

        let parent_mode_id = self
            .sessions
            .get(&src_id)
            .map(|session| session.current_mode_id.clone())
            .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
        let parent_budget = self
            .sessions
            .get(&src_id)
            .map(|session| session.budget.clone())
            .unwrap_or_default();
        // A fork is the same session lineage: it inherits the parent's
        // capability profile (and thus its grants), not a fresh legacy env.
        let parent_profile = self
            .sessions
            .get(&src_id)
            .and_then(|session| session.capability_profile.clone());
        let cancellation = self.register_session_cancellation(&new_session_id);
        let fork_cwd = harn_vm::agent_sessions::workspace_anchor(&new_session_id)
            .map(|anchor| anchor.primary)
            .unwrap_or(src_cwd);
        let project_root = session_project_root_for_cwd(&fork_cwd);
        self.sessions.insert(
            new_session_id.clone(),
            Session {
                cwd: fork_cwd,
                project_root,
                cancellation,
                host_bridge: None,
                inject_state: harn_vm::bridge::HostBridgeInjectionState::default(),
                info: info.clone(),
                advertised_commands: Vec::new(),
                current_mode_id: parent_mode_id.clone(),
                budget: parent_budget,
                profile_turn: 0,
                capability_profile: parent_profile,
            },
        );
        self.emit_session_info_update(&new_session_id, &info);
        self.emit_available_commands(&new_session_id);
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": new_session_id,
                "state": "forked",
                "parent_id": src_id,
                "branched_at": branched_at,
                "modes": modes::session_mode_state(&parent_mode_id),
                "configOptions": self.config_options_for_session(&new_session_id, &parent_mode_id),
            }),
        );
    }

    pub(super) fn handle_session_truncate(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "Missing sessionId");
            return;
        };
        let keep_first =
            match nonnegative_usize_param(params, &["keepFirst", "keep_first"], "keepFirst") {
                Ok(Some(value)) => value,
                Ok(None) => {
                    self.send_error(id, -32602, "Missing keepFirst");
                    return;
                }
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    return;
                }
            };
        let Some(cancellation) = self
            .sessions
            .get(&session_id)
            .map(|session| session.cancellation.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        };

        cancellation.cancel();
        if !harn_vm::agent_sessions::exists(&session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        }
        let Some(result) = harn_vm::agent_sessions::truncate(&session_id, keep_first) else {
            self.send_error(
                id,
                -32000,
                &format!("Failed to truncate session: {session_id}"),
            );
            return;
        };

        let mut update = serde_json::json!({
            "sessionUpdate": "session_truncated",
            "keptTurnCount": result.kept_turn_count,
            "removedTurnCount": result.removed_turn_count,
            "newTipTurnId": result.new_tip_turn_id,
        });
        if let Some(reason) = params.get("reason").and_then(|value| value.as_str()) {
            update["reason"] = serde_json::json!(reason);
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "keptTurnCount": result.kept_turn_count,
                "removedTurnCount": result.removed_turn_count,
                "newTipTurnId": result.new_tip_turn_id,
            }),
        );
    }
}
