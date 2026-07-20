use super::*;

impl AcpServer {
    #[cfg(feature = "hostlib")]
    pub(super) fn handle_session_fs_mode(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/fs_mode requires sessionId");
            return;
        };
        let Some(mode_raw) = params.get("mode").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/fs_mode requires mode");
            return;
        };
        let mode = match mode_raw {
            "immediate" => harn_hostlib::fs::FsMode::Immediate,
            "staged" => harn_hostlib::fs::FsMode::Staged,
            other => {
                self.send_error(
                    id,
                    -32602,
                    &format!("session/fs_mode mode must be immediate or staged, got {other}"),
                );
                return;
            }
        };
        let Some(cwd) = self
            .sessions
            .get(session_id)
            .map(|session| session.cwd.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        };
        match harn_hostlib::fs::set_mode(session_id, mode, Some(&cwd)) {
            Ok(result) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "previousMode": result.previous_mode.as_str(),
                        "mode": mode.as_str(),
                    }),
                );
                self.emit_staged_writes_update(session_id);
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    pub(super) fn handle_session_fs_mode(
        &mut self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) {
        self.send_error(id, -32601, "session/fs_mode requires the hostlib feature");
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn handle_session_fs_commit_staged(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/fs_commit_staged requires sessionId");
            return;
        };
        if !self.sessions.contains_key(session_id.as_str()) {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        }
        let paths = staged_fs_paths_param(params);
        match harn_hostlib::fs::commit_staged(session_id.as_str(), &paths) {
            Ok(result) => {
                harn_vm::agent_sessions::invalidate_redo(session_id.as_str());
                self.send_response(
                    id,
                    serde_json::json!({
                        "committedPaths": result.committed_paths,
                        "failedPathsWithReasons": result
                            .failed_paths_with_reasons
                            .into_iter()
                            .map(|(path, reason)| serde_json::json!({
                                "path": path,
                                "reason": reason,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                );
                self.emit_staged_writes_update(session_id.as_str());
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    pub(super) fn handle_session_fs_commit_staged(
        &mut self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) {
        self.send_error(
            id,
            -32601,
            "session/fs_commit_staged requires the hostlib feature",
        );
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn handle_session_fs_discard_staged(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/fs_discard_staged requires sessionId");
            return;
        };
        if !self.sessions.contains_key(session_id.as_str()) {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        }
        let paths = staged_fs_paths_param(params);
        match harn_hostlib::fs::discard_staged(session_id.as_str(), &paths) {
            Ok(result) => {
                harn_vm::agent_sessions::invalidate_redo(session_id.as_str());
                self.send_response(
                    id,
                    serde_json::json!({
                        "discardedPaths": result.discarded_paths,
                    }),
                );
                self.emit_staged_writes_update(session_id.as_str());
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    pub(super) fn handle_session_fs_discard_staged(
        &mut self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) {
        self.send_error(
            id,
            -32601,
            "session/fs_discard_staged requires the hostlib feature",
        );
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn handle_session_restore_tool_call(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/restore_tool_call requires sessionId");
            return;
        };
        let Some(tool_call_id) = params
            .get("toolCallId")
            .or_else(|| params.get("tool_call_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/restore_tool_call requires toolCallId");
            return;
        };
        if !self.sessions.contains_key(session_id) {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        }
        let paths = params
            .get("paths")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match harn_hostlib::fs_snapshot::restore(session_id, tool_call_id, &paths) {
            Ok(result) => {
                harn_vm::agent_sessions::invalidate_redo(session_id);
                self.send_response(
                    id,
                    serde_json::json!({
                        "toolCallId": &result.snapshot_id,
                        "restoredPaths": &result.restored_paths,
                        "skippedPathsWithReasons": result
                            .skipped_paths_with_reasons
                            .iter()
                            .map(|(path, reason)| serde_json::json!({
                                "path": path,
                                "reason": reason,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                );
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": &result.snapshot_id,
                    "status": "restored",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "kind".to_string(),
                    serde_json::Value::String("tool_call_restored".to_string()),
                );
                harn_meta.insert(
                    "restoredPaths".to_string(),
                    serde_json::to_value(&result.restored_paths).unwrap_or_default(),
                );
                if !result.skipped_paths_with_reasons.is_empty() {
                    harn_meta.insert(
                        "skippedPathsWithReasons".to_string(),
                        serde_json::to_value(
                            result
                                .skipped_paths_with_reasons
                                .iter()
                                .map(|(path, reason)| {
                                    serde_json::json!({
                                        "path": path,
                                        "reason": reason,
                                    })
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                    );
                }
                events::merge_harn_meta(&mut update, harn_meta);
                self.send_notification(
                    "session/update",
                    serde_json::json!({
                        "sessionId": session_id,
                        "update": update,
                    }),
                );
            }
            Err(error) => self.send_error(id, -32000, &error.to_string()),
        }
    }

    #[cfg(not(feature = "hostlib"))]
    pub(super) fn handle_session_restore_tool_call(
        &mut self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) {
        self.send_error(
            id,
            -32601,
            "session/restore_tool_call requires the hostlib feature",
        );
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn restore_fs_snapshots(
        &self,
        session_id: &str,
        snapshot_ids: &[String],
        reverse: bool,
    ) -> Result<Vec<serde_json::Value>, String> {
        let mut ids = snapshot_ids.to_vec();
        if reverse {
            ids.reverse();
        }
        // TEMP DIAGNOSTIC (windows-nightly acp:1299) — remove before merge.
        eprintln!("[diag-acp-restore] session={session_id} ids={ids:?} reverse={reverse}");
        let mut restored = Vec::new();
        for snapshot_id in ids {
            let result = harn_hostlib::fs_snapshot::restore(session_id, &snapshot_id, &[])
                .map_err(|error| error.to_string())?;
            restored.push(serde_json::json!({
                "snapshotId": result.snapshot_id,
                "restoredPaths": result.restored_paths,
                "skippedPathsWithReasons": result
                    .skipped_paths_with_reasons
                    .into_iter()
                    .map(|(path, reason)| serde_json::json!({
                        "path": path,
                        "reason": reason,
                    }))
                    .collect::<Vec<_>>(),
            }));
        }
        Ok(restored)
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn capture_redo_snapshots(
        &self,
        session_id: &str,
        checkpoint_id: &str,
        rollback_snapshot_ids: &[String],
    ) -> Result<Vec<String>, String> {
        let Some(cwd) = self
            .sessions
            .get(session_id)
            .map(|session| session.cwd.clone())
        else {
            return Err(format!("Unknown session: {session_id}"));
        };
        let summaries = harn_hostlib::fs_snapshot::list_snapshots(session_id)
            .map_err(|error| error.to_string())?;
        let by_id: HashMap<String, harn_hostlib::fs_snapshot::SnapshotSummary> = summaries
            .into_iter()
            .map(|summary| (summary.snapshot_id.clone(), summary))
            .collect();
        // TEMP DIAGNOSTIC (windows-nightly acp:1299) — remove before merge.
        eprintln!(
            "[diag-acp-redo] cwd={cwd:?} rollback_snapshot_ids={rollback_snapshot_ids:?} known_ids={:?}",
            by_id.keys().collect::<Vec<_>>()
        );
        let mut redo_ids = Vec::new();
        for snapshot_id in rollback_snapshot_ids {
            let Some(summary) = by_id.get(snapshot_id) else {
                continue;
            };
            if summary.captured_paths.is_empty() {
                continue;
            }
            let redo_id = format!("{checkpoint_id}:redo:{snapshot_id}");
            // TEMP DIAGNOSTIC (windows-nightly acp:1299) — remove before merge.
            eprintln!(
                "[diag-acp-redo] snapshotting redo_id={redo_id:?} captured_paths={:?}",
                summary.captured_paths
            );
            harn_hostlib::fs_snapshot::snapshot(
                session_id,
                &redo_id,
                &summary.captured_paths,
                Some(&cwd),
            )
            .map_err(|error| error.to_string())?;
            redo_ids.push(redo_id);
        }
        Ok(redo_ids)
    }

    pub(super) fn checkpoint_response(
        &self,
        outcome: harn_vm::agent_sessions::SessionCheckpointOutcome,
        fs_restores: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        serde_json::json!({
            "status": outcome.status,
            "checkpointId": outcome.checkpoint.checkpoint_id,
            "beforeMessageCount": outcome.checkpoint.before_message_count,
            "afterMessageCount": outcome.checkpoint.after_message_count,
            "fsSnapshotIds": outcome.checkpoint.fs_snapshot_ids,
            "redoFsSnapshotIds": outcome.redo_fs_snapshot_ids,
            "fsRestores": fs_restores,
        })
    }

    pub(super) fn send_checkpoint_error(
        &self,
        id: &serde_json::Value,
        code: i64,
        status: &'static str,
        message: &str,
    ) {
        // TEMP DIAGNOSTIC (windows-nightly acp:1299) — remove before merge.
        eprintln!("[diag-acp-checkpoint-error] code={code} status={status} message={message}");
        self.send_error_with_data(id, code, message, serde_json::json!({ "status": status }));
    }

    pub(super) fn handle_session_rollback(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_checkpoint_error(
                id,
                -32602,
                "invalid_params",
                "session/rollback requires sessionId",
            );
            return;
        };
        let Some(session) = self.sessions.get(&session_id) else {
            self.send_checkpoint_error(
                id,
                -32602,
                "unknown_session",
                &format!("Unknown session: {session_id}"),
            );
            return;
        };
        if session.host_bridge.is_some() {
            self.send_checkpoint_error(
                id,
                -32000,
                "prompt_active",
                "session/rollback rejected: prompt is active",
            );
            return;
        }
        let plan = match harn_vm::agent_sessions::rollback_plan(&session_id) {
            Ok(plan) => plan,
            Err(error) => {
                let status = harn_vm::agent_sessions::checkpoint_status_name(error);
                self.send_checkpoint_error(id, -32000, status, status);
                return;
            }
        };
        #[cfg(not(feature = "hostlib"))]
        let _ = &plan;
        #[cfg(feature = "hostlib")]
        let redo_fs_snapshot_ids = match self.capture_redo_snapshots(
            &session_id,
            &plan.checkpoint_id,
            &plan.fs_snapshot_ids,
        ) {
            Ok(ids) => ids,
            Err(message) => {
                self.send_checkpoint_error(id, -32000, "fs_snapshot_error", &message);
                return;
            }
        };
        #[cfg(not(feature = "hostlib"))]
        let redo_fs_snapshot_ids = Vec::new();
        #[cfg(feature = "hostlib")]
        let fs_restores = match self.restore_fs_snapshots(&session_id, &plan.fs_snapshot_ids, true)
        {
            Ok(restores) => restores,
            Err(message) => {
                self.send_checkpoint_error(id, -32000, "fs_restore_error", &message);
                return;
            }
        };
        #[cfg(not(feature = "hostlib"))]
        let fs_restores = Vec::new();
        let outcome = match harn_vm::agent_sessions::rollback_last_completed_turn(
            &session_id,
            redo_fs_snapshot_ids,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let status = harn_vm::agent_sessions::checkpoint_status_name(error);
                self.send_checkpoint_error(id, -32000, status, status);
                return;
            }
        };
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "session_rollback",
                    "checkpointId": outcome.checkpoint.checkpoint_id,
                    "status": outcome.status,
                },
            }),
        );
        self.send_response(id, self.checkpoint_response(outcome, fs_restores));
    }

    pub(super) fn handle_session_redo(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_checkpoint_error(
                id,
                -32602,
                "invalid_params",
                "session/redo requires sessionId",
            );
            return;
        };
        let Some(session) = self.sessions.get(&session_id) else {
            self.send_checkpoint_error(
                id,
                -32602,
                "unknown_session",
                &format!("Unknown session: {session_id}"),
            );
            return;
        };
        if session.host_bridge.is_some() {
            self.send_checkpoint_error(
                id,
                -32000,
                "prompt_active",
                "session/redo rejected: prompt is active",
            );
            return;
        }
        let plan = match harn_vm::agent_sessions::redo_plan(&session_id) {
            Ok(plan) => plan,
            Err(error) => {
                let status = harn_vm::agent_sessions::checkpoint_status_name(error);
                self.send_checkpoint_error(id, -32000, status, status);
                return;
            }
        };
        #[cfg(not(feature = "hostlib"))]
        let _ = &plan;
        #[cfg(feature = "hostlib")]
        let fs_restores = match self.restore_fs_snapshots(&session_id, &plan.fs_snapshot_ids, false)
        {
            Ok(restores) => restores,
            Err(message) => {
                self.send_checkpoint_error(id, -32000, "fs_restore_error", &message);
                return;
            }
        };
        #[cfg(not(feature = "hostlib"))]
        let fs_restores = Vec::new();
        let outcome = match harn_vm::agent_sessions::redo_last_rollback(&session_id) {
            Ok(outcome) => outcome,
            Err(error) => {
                let status = harn_vm::agent_sessions::checkpoint_status_name(error);
                self.send_checkpoint_error(id, -32000, status, status);
                return;
            }
        };
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "session_redo",
                    "checkpointId": outcome.checkpoint.checkpoint_id,
                    "status": outcome.status,
                },
            }),
        );
        self.send_response(id, self.checkpoint_response(outcome, fs_restores));
    }

    pub(super) fn handle_session_set_mode(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/set_mode requires sessionId");
            return;
        };
        let Some(mode_id) = params
            .get("modeId")
            .or_else(|| params.get("mode_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/set_mode requires modeId");
            return;
        };

        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(id, serde_json::json!({}));
                if changed {
                    self.emit_current_mode_update(session_id, mode_id);
                    self.emit_config_option_update(session_id, mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    pub(super) fn handle_session_set_config_option(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires sessionId");
            return;
        };
        let Some(config_id) = params.get("configId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires configId");
            return;
        };
        let Some(value) = params.get("value").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires value");
            return;
        };

        let session_id = session_id.to_string();
        match config_id {
            "mode" => self.apply_set_mode_config_option(id, &session_id, value),
            "model" => self.apply_set_model_config_option(id, &session_id, value),
            "thought_level" | "reasoning_policy" => {
                self.apply_set_reasoning_policy_config_option(id, &session_id, value);
            }
            "budget" => self.apply_set_budget_config_option(id, &session_id, value),
            other => self.send_error(
                id,
                -32602,
                &format!(
                    "Unknown config option '{other}'. Available: mode, model, thought_level, budget"
                ),
            ),
        }
    }

    pub(super) fn apply_set_mode_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        mode_id: &str,
    ) {
        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, mode_id),
                    }),
                );
                if changed {
                    self.emit_current_mode_update(session_id, mode_id);
                    self.emit_config_option_update(session_id, mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    pub(super) fn apply_set_model_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let normalized = match modes::validate_model_selector(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_model(session_id, normalized) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, &current_mode_id),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    pub(super) fn apply_set_reasoning_policy_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let normalized = match modes::validate_reasoning_policy_selector(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_reasoning_policy(session_id, normalized) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, &current_mode_id),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    pub(super) fn apply_set_budget_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let budget = match parse_budget_config_value(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_budget(session_id, budget) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": self.config_options_for_session(session_id, &current_mode_id),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }
}
