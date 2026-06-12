use super::*;

impl AcpServer {
    /// Dispatch one incoming ACP JSON-RPC message.
    ///
    /// The same router backs stdio, WebSocket, and in-process channel
    /// transports. `msg` must be either a request/notification with `method`
    /// or a response with `id` for a pending host callback.
    pub async fn handle_incoming_message(&mut self, msg: serde_json::Value) {
        if msg.get("method").is_none() && msg.get("id").is_some() {
            if let Some(id) = msg["id"].as_u64() {
                let mut pending = self.pending.lock().await;
                if let Some(sender) = pending.remove(&id) {
                    let _ = sender.send(msg);
                }
            }
            return;
        }

        let method = match msg.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return,
        };
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

        match method.as_str() {
            "initialize" => {
                self.handle_initialize(&id);
            }
            "authenticate" => {
                self.handle_authenticate(&id, &params).await;
            }
            HARN_PROVIDER_CATALOG_METHOD => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_provider_catalog(&id);
            }
            harn_vm::session_timeline::SESSION_TIMELINE_QUERY_METHOD => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_timeline_query(&id, &params).await;
            }
            harn_vm::session_timeline::SESSION_TIMELINE_SUBSCRIBE_METHOD => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_timeline_subscribe(&id, &params).await;
            }
            harn_vm::session_timeline::SESSION_TIMELINE_UNSUBSCRIBE_METHOD => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_timeline_unsubscribe(&id, &params);
            }
            "session/new" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_new(&id, &params);
            }
            "session/load" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_load(&id, &params).await;
            }
            "session/resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_resume(&id, &params);
            }
            "session/fork" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fork(&id, &params);
            }
            "session/truncate" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_truncate(&id, &params);
            }
            "session/set_mode" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_set_mode(&id, &params);
            }
            "session/set_config_option" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_set_config_option(&id, &params);
            }
            "session/fs_mode" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fs_mode(&id, &params);
            }
            "session/fs_commit_staged" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fs_commit_staged(&id, &params);
            }
            "session/fs_discard_staged" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fs_discard_staged(&id, &params);
            }
            "session/restore_tool_call" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_restore_tool_call(&id, &params);
            }
            "session/rollback" | "harn.session_rollback" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_rollback(&id, &params);
            }
            "session/redo" | "harn.session_redo" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_redo(&id, &params);
            }
            "session/prompt" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_prompt(&id, &params).await;
            }
            "session/cancel" => {
                // `reject_unauthenticated` only answers when `id` is non-null,
                // so the notification form is silently ignored when
                // unauthenticated rather than processed.
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_cancel(&id, &params);
            }
            "session/cancel_tool_call" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_cancel_tool_call(&id, &params);
            }
            "session/close" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_close(&id, &params, "session/close");
            }
            "session/stop" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                tracing::warn!("ACP method session/stop is deprecated; use session/close instead");
                eprintln!(
                    "warning: ACP method session/stop is deprecated; use session/close instead"
                );
                self.handle_session_close(&id, &params, "session/stop");
            }
            "session/inject" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_inject(&id, &params).await;
            }
            "session/revoke_inject" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_revoke_inject(&id, &params).await;
            }
            "session/replace_inject" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_replace_inject(&id, &params).await;
            }
            "session/remind" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_remind(&id, &params).await;
            }
            "session/pending_injections" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_pending_injections(&id, &params).await;
            }
            "session/revoke_reminder" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_revoke_reminder(&id, &params).await;
            }
            "agent/resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_agent_resume(&params);
            }
            "harn.hitl.respond" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_hitl_respond(&id, &params).await;
            }
            "workflow/signal" | "harn.workflow.signal" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_signal(&id, &params).await;
            }
            "workflow/query" | "harn.workflow.query" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_query(&id, &params);
            }
            "workflow/update" | "harn.workflow.update" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_update(&id, &params).await;
            }
            "workflow/pause" | "harn.workflow.pause" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_pause(&id, &params);
            }
            "workflow/resume" | "harn.workflow.resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_resume(&id, &params);
            }
            "session/list" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_list(&id, &params);
            }
            "harn.session_workspace_roots" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_harn_session_workspace_roots(&id, &params);
            }
            "harn.session_add_root" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_harn_session_add_root(&id, &params);
            }
            "harn.session_reanchor" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_harn_session_reanchor(&id, &params);
            }
            "mcp/catalog" | "harn.mcp.catalog" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_catalog(&id, &params);
            }
            "mcp/authorize" | "harn.mcp.authorize" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_authorize(&id, &params).await;
            }
            "mcp/oauth_callback" | "harn.mcp.oauth_callback" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_oauth_callback(&id, &params).await;
            }
            "mcp/import_token" | "harn.mcp.import_token" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_mcp_import_token(&id, &params).await;
            }
            _ => {
                if !id.is_null() {
                    self.send_error(&id, -32601, &format!("Method not found: {method}"));
                }
            }
        }
    }
}
