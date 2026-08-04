use super::*;

impl AcpServer {
    pub(super) fn session_restore_result(&self, session_id: &str) -> Option<serde_json::Value> {
        let session = self.sessions.get(session_id)?;
        let session_value = self.session_item_json(session_id, "live", None)?;
        Some(serde_json::json!({
            "sessionId": session_id,
            "session": session_value,
            "modes": modes::session_mode_state(&session.current_mode_id),
            "configOptions": self.config_options_for_session(session_id, &session.current_mode_id),
        }))
    }

    pub(super) fn session_item_json(
        &self,
        session_id: &str,
        live_state: &str,
        last_event_id: Option<u64>,
    ) -> Option<serde_json::Value> {
        let session = self.sessions.get(session_id)?;
        let workspace_anchor = harn_vm::agent_sessions::workspace_anchor(session_id);
        let snapshot = harn_vm::agent_sessions::snapshot(session_id)
            .map(|value| harn_vm::llm::vm_value_to_json(&value))
            .unwrap_or(serde_json::Value::Null);
        let active_prompt = session.host_bridge.is_some();
        let attachable_roles = serde_json::json!(["host_owner"]);
        let mut item = serde_json::json!({
            "sessionId": session_id,
            "cwd": session.cwd.display().to_string(),
            "liveState": live_state,
            "attachableRoles": attachable_roles,
            "currentModeId": session.current_mode_id,
            "activePrompt": active_prompt,
        });
        if let Some(created_at) = snapshot.get("created_at").cloned() {
            item["createdAt"] = created_at;
        }
        if let Some(last_event_id) = last_event_id {
            item["lastEventId"] = serde_json::json!(last_event_id);
        }
        if let Some(title) = session.info.title.as_ref() {
            item["title"] = serde_json::json!(title);
        }
        if let Some(anchor) = workspace_anchor.as_ref() {
            item["workspaceAnchor"] = anchor.to_json();
        }

        let mut meta = session.info.meta.clone();
        let mut harn_meta = match meta.remove("harn") {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        harn_meta.insert("liveState".to_string(), serde_json::json!(live_state));
        harn_meta.insert(
            "attachableRoles".to_string(),
            item["attachableRoles"].clone(),
        );
        harn_meta.insert(
            "currentModeId".to_string(),
            serde_json::json!(session.current_mode_id),
        );
        harn_meta.insert("activePrompt".to_string(), serde_json::json!(active_prompt));
        if let Some(last_event_id) = last_event_id {
            harn_meta.insert("lastEventId".to_string(), serde_json::json!(last_event_id));
        }
        if let Some(anchor) = workspace_anchor {
            harn_meta.insert("workspaceAnchor".to_string(), anchor.to_json());
        }
        meta.insert("harn".to_string(), serde_json::Value::Object(harn_meta));
        item["_meta"] = serde_json::Value::Object(meta);
        Some(item)
    }

    pub(super) fn session_matches_list_filters(
        &self,
        session_id: &str,
        session: &Session,
        params: &serde_json::Value,
    ) -> bool {
        if let Some(cwd) = session_cwd_filter(params) {
            if session.cwd.to_string_lossy() != cwd {
                return false;
            }
        }
        let live_state = "live";
        let state_filter = session_live_state_filter(params);
        if !live_state_filter_matches(live_state, state_filter.as_deref()) {
            return false;
        }
        let workspace_anchor = harn_vm::agent_sessions::workspace_anchor(session_id);
        workspace_anchor_filter_matches(
            workspace_anchor.as_ref(),
            session_workspace_anchor_filter(params),
        )
    }

    /// Extract the request's `sessionId`/`session_id` parameter, emitting a
    /// JSON-RPC error and returning `None` when it is absent or non-string.
    pub(super) fn session_id_param<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let session_id = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str);
        if session_id.is_none() {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
        }
        session_id
    }

    pub(super) fn restored_session_id<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let session_id = self.session_id_param(id, params, method)?;

        if !self.sessions.contains_key(session_id) {
            self.send_error(id, -32602, &format!("unknown session: {session_id}"));
            return None;
        }

        harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        Some(session_id)
    }

    /// Register a session the live server never saw so its persisted
    /// replay history can be restored into an interactive session. Used
    /// by `session/load` when a client (e.g. the Rust TUI saved-session
    /// picker) attaches a host-saved session after a fresh `harn serve`
    /// start. The in-process channel server spins prompt turns up on
    /// demand, so the restored session is genuinely live — unlike the
    /// WebSocket hub's `expired_replay_only` fallback, which has no
    /// worker to re-attach to.
    pub(super) fn register_restored_session(
        &mut self,
        session_id: &str,
        params: &serde_json::Value,
    ) {
        let cwd = params
            .get("cwd")
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        self.insert_session(session_id.to_string(), cwd, SessionInfo::default());
    }

    pub(super) async fn handle_session_load(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = self
            .session_id_param(id, params, "session/load")
            .map(str::to_owned)
        else {
            return;
        };

        if let Err(error) = flush_session_sinks(&session_id).await {
            self.send_error(
                id,
                -32000,
                &format!("Failed to persist session {session_id} before replay: {error}"),
            );
            return;
        }

        // Replay events are the durable source of truth for the in-process
        // path, so load them before deciding whether the session is
        // restorable. This mirrors the WebSocket hub's persisted fallback in
        // `replay_persisted_acp_events`, but keeps the restored session live
        // and promptable rather than replay-only.
        let replay_events =
            match harn_vm::orchestration::load_agent_session_replay_events(&session_id).await {
                Ok(events) => events,
                Err(error) => {
                    self.send_error(
                        id,
                        -32000,
                        &format!("Failed to replay session {session_id}: {error}"),
                    );
                    return;
                }
            };

        if self.sessions.contains_key(&session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        } else if replay_events.is_empty() {
            // No live session and nothing persisted under this id: fail loudly
            // so the client can distinguish a typo/stale id from a real load.
            self.send_error(id, -32602, &format!("unknown session: {session_id}"));
            return;
        } else {
            self.register_restored_session(&session_id, params);
        }

        let replay_sink = AcpAgentEventSink::for_replay(self.output.clone());
        for replay_event in &replay_events {
            replay_sink.handle_event(&replay_event.event);
        }
        let replayed: Vec<serde_json::Value> = replay_events
            .iter()
            .map(|event| {
                serde_json::json!({
                    "eventId": event.event_id,
                    "type": serde_json::to_value(&event.event)
                        .ok()
                        .and_then(|value| value.get("type").cloned())
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();

        let mut result = self
            .session_restore_result(&session_id)
            .expect("validated session should still exist");
        result["replayed"] = serde_json::json!(replayed);
        self.send_response(id, result);
    }

    pub(super) fn handle_session_resume(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = self.restored_session_id(id, params, "session/resume") else {
            return;
        };
        let result = self
            .session_restore_result(session_id)
            .expect("validated session should still exist");
        self.send_response(id, result);
    }

    pub(super) fn set_session_mode(
        &mut self,
        session_id: &str,
        mode_id: &str,
    ) -> Result<bool, String> {
        if !modes::is_known(mode_id) {
            return Err(format!(
                "Unknown mode '{mode_id}'. Available: {}",
                modes::known_mode_ids().join(", ")
            ));
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        if session.current_mode_id == mode_id {
            return Ok(false);
        }
        session.current_mode_id = mode_id.to_string();
        Ok(true)
    }

    /// Pin (or clear, with `None`) the LLM model selector for `session_id`.
    /// Returns `Ok(true)` when the value changed so callers can decide
    /// whether to broadcast a `config_option_update` notification.
    ///
    /// The harn-vm session is auto-created if it doesn't exist yet (e.g.
    /// when a client pins a model before its first prompt), keeping
    /// the wire surface order-independent.
    pub(super) fn set_session_model(
        &mut self,
        session_id: &str,
        model: Option<String>,
    ) -> Result<bool, String> {
        if !self.sessions.contains_key(session_id) {
            return Err(format!("Unknown session: {session_id}"));
        }
        if !harn_vm::agent_sessions::exists(session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        }
        harn_vm::agent_sessions::set_pinned_model(session_id, model)
    }

    /// Read the currently pinned model for `session_id`, if any. Returns
    /// `None` for unknown sessions or sessions running on the ambient
    /// default — both are indistinguishable on the wire.
    pub(super) fn pinned_model(&self, session_id: &str) -> Option<String> {
        harn_vm::agent_sessions::pinned_model(session_id)
    }

    /// Pin (or clear) the provider-aware reasoning policy for `session_id`.
    pub(super) fn set_session_reasoning_policy(
        &mut self,
        session_id: &str,
        policy: Option<String>,
    ) -> Result<bool, String> {
        if !self.sessions.contains_key(session_id) {
            return Err(format!("Unknown session: {session_id}"));
        }
        if !harn_vm::agent_sessions::exists(session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        }
        harn_vm::agent_sessions::set_pinned_reasoning_policy(session_id, policy)
    }

    pub(super) fn pinned_reasoning_policy(&self, session_id: &str) -> Option<String> {
        harn_vm::agent_sessions::pinned_reasoning_policy(session_id)
    }

    pub(super) fn session_budget_config_value(&self, session_id: &str) -> Option<String> {
        match &self.sessions.get(session_id)?.budget {
            SessionBudget::Inherit => None,
            SessionBudget::Unlimited => Some(modes::BUDGET_OFF_VALUE.to_string()),
            SessionBudget::Custom(spec) => Some(budget_config_value(spec)),
        }
    }

    pub(super) fn config_options_for_session(
        &self,
        session_id: &str,
        mode_id: &str,
    ) -> serde_json::Value {
        let budget_value = self.session_budget_config_value(session_id);
        modes::config_options_state(
            mode_id,
            self.pinned_model(session_id).as_deref(),
            self.pinned_reasoning_policy(session_id).as_deref(),
            budget_value.as_deref(),
        )
    }

    pub(super) fn set_session_budget(
        &mut self,
        session_id: &str,
        budget: SessionBudget,
    ) -> Result<bool, String> {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        if session.budget == budget {
            return Ok(false);
        }
        session.budget = budget;
        Ok(true)
    }

    pub(super) fn emit_current_mode_update(&self, session_id: &str, mode_id: &str) {
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "current_mode_update",
                    "modeId": mode_id,
                },
            }),
        );
    }

    pub(super) fn emit_config_option_update(&self, session_id: &str, mode_id: &str) {
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": self.config_options_for_session(session_id, mode_id),
                },
            }),
        );
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn emit_staged_writes_update(&self, session_id: &str) {
        let Ok(status) = harn_hostlib::fs::staged_status(session_id) else {
            return;
        };
        let mut update = bridge::progress_update(
            "fs_staging",
            "staged writes pending",
            Some(status.pending_writes.len() as i64),
            None,
            None,
        );
        let pending_writes = status
            .pending_writes
            .iter()
            .map(harn_hostlib::fs::PendingWrite::event_summary)
            .collect::<Vec<_>>();
        events::merge_harn_meta(
            &mut update,
            staged_writes::harn_meta(
                pending_writes.len(),
                status.total_bytes_pending,
                &pending_writes,
            ),
        );
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
    }
}
