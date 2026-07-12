use super::*;

impl AcpServer {
    pub(super) fn handle_session_inject_host_event(
        &self,
        id: &serde_json::Value,
        params: serde_json::Value,
    ) {
        let params: AcpSessionInjectHostEventParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("{ACP_METHOD_SESSION_INJECT_HOST_EVENT}: invalid params: {error}"),
                );
                return;
            }
        };
        match harn_vm::agent_sessions::inject_host_event_request(&params.session_id, params.event) {
            Ok(result) => self.send_response(id, result),
            Err(error) if error.contains("unknown session id") => {
                self.send_error(id, -32004, &error);
            }
            Err(error) => self.send_error(id, -32602, &error),
        }
    }

    pub(super) fn handle_session_cancel(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            if !id.is_null() {
                self.send_error(id, -32602, "session/cancel requires sessionId");
            }
            return;
        };
        let Some(cancellation) =
            lookup_session_cancellation(&self.session_cancellations, &session_id)
        else {
            if !id.is_null() {
                self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            }
            return;
        };

        let actor = control_actor_from_params(params);
        let newly_cancelled = if cancellation.take_routed_cancel_ack() {
            true
        } else {
            cancellation.cancel()
        };
        let status = if newly_cancelled {
            "cancelled"
        } else {
            "already_cancelled"
        };
        self.emit_control_outcome(
            &session_id,
            "session/cancel",
            status,
            "accepted",
            actor.clone(),
            serde_json::json!({"sessionId": session_id}),
            None,
        );
        if !id.is_null() {
            self.send_response(
                id,
                serde_json::json!({
                    "sessionId": session_id,
                    "status": status,
                    "_meta": {
                        "harn": {
                            "actor": actor,
                        }
                    }
                }),
            );
        }
    }

    /// Targeted preemption: stop one in-flight tool call without tearing
    /// down the whole session. Mirrors the `cancel_in_flight_tool_call`
    /// Harn builtin so hosts have a single semantic across protocol and
    /// in-VM call sites.
    pub(super) fn handle_session_cancel_tool_call(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "session/cancel_tool_call requires sessionId");
            return;
        };
        let call_id = params
            .get("toolCallId")
            .or_else(|| params.get("tool_call_id"))
            .or_else(|| params.get("callId"))
            .or_else(|| params.get("call_id"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if call_id.is_empty() {
            self.send_error(id, -32602, "session/cancel_tool_call requires toolCallId");
            return;
        }
        let reason = params
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("host cancelled in-flight tool call")
            .to_string();
        let inject_reminder = params
            .get("injectReminder")
            .or_else(|| params.get("inject_reminder"))
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        let outcome =
            harn_vm::tool_call_cancellations::cancel(session_id, call_id, reason, inject_reminder);
        let tool_name = outcome
            .tool_name
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        self.send_response(
            id,
            serde_json::json!({
                "status": outcome.status.as_str(),
                "callId": call_id,
                "tool": tool_name,
            }),
        );
    }

    pub(super) fn handle_session_close(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &str,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return;
        };

        let Some(session) = self.sessions.remove(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return;
        };

        session.cancellation.cancel();
        self.session_cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
        self.inject_controls.remove(session_id);
        self.timeline_subscriptions.retain(|_, subscription| {
            if subscription.session_id.as_deref() == Some(session_id) {
                subscription.handle.abort();
                false
            } else {
                true
            }
        });
        clear_session_sinks(session_id);
        #[cfg(feature = "hostlib")]
        {
            harn_hostlib::fs_snapshot::drop_session_snapshots(session_id);
        }
        harn_vm::agent_sessions::close_with_status(
            session_id,
            "client_request",
            "closed",
            serde_json::json!({
                "protocol": "acp",
                "method": method,
            }),
        );

        self.send_response(id, serde_json::json!({}));
    }

    pub(super) async fn handle_session_inject(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/inject requires sessionId");
            return;
        };
        let Some(inject_state) = self.session_inject_state(id, params, "session/inject", true)
        else {
            return;
        };
        let actor = control_actor_from_params(params);
        let mode = match bridge_mode_for_session_inject(params) {
            Ok(mode) => mode,
            Err(message) => {
                self.send_error(id, -32602, &message);
                self.emit_control_outcome(
                    &session_id,
                    "session/inject",
                    "rejected",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id}),
                    Some("invalid_mode"),
                );
                return;
            }
        };
        let (content, transcript_content) =
            match normalize_session_inject_content("session/inject", params) {
                Ok(content) => content,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    self.emit_control_outcome(
                        &session_id,
                        "session/inject",
                        "rejected",
                        "rejected",
                        actor,
                        serde_json::json!({"sessionId": session_id}),
                        Some("invalid_content"),
                    );
                    return;
                }
            };
        let message_id = inject_state
            .push_pending_user_message(content, transcript_content, mode)
            .await;
        self.inject_controls
            .entry(session_id.clone())
            .or_default()
            .insert(
                message_id.clone(),
                InjectControlRecord {
                    owner: actor.clone(),
                },
            );
        self.emit_control_outcome(
            &session_id,
            "session/inject",
            "accepted",
            "accepted",
            actor.clone(),
            serde_json::json!({"sessionId": session_id, "messageId": message_id}),
            None,
        );
        self.send_response(
            id,
            serde_json::json!({
                "messageId": message_id,
                "status": "accepted",
                "_meta": {
                    "harn": {
                        "actor": actor,
                    }
                }
            }),
        );
    }

    pub(super) fn clear_active_prompt_transport(&mut self, session_id: &str) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.host_bridge = None;
        }
        clear_session_sinks(session_id);
    }

    pub(super) async fn handle_session_revoke_inject(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/revoke_inject requires sessionId");
            return;
        };
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/revoke_inject", false)
        else {
            return;
        };
        let Some(message_id) =
            self.pending_inject_message_id_param(id, params, "session/revoke_inject")
        else {
            return;
        };
        let actor = control_actor_from_params(params);
        if let Some(owner) = self
            .inject_controls
            .get(&session_id)
            .and_then(|records| records.get(message_id))
            .map(|record| record.owner.clone())
        {
            if owner != actor && !actor_is_host_owner(&actor) {
                self.send_pending_inject_error_with_data(
                    id,
                    message_id,
                    "not_owner_or_not_authorized",
                    "pending inject is owned by another ACP actor",
                    serde_json::json!({
                        "actor": actor,
                        "owner": owner,
                    }),
                );
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "rejected",
                    "rejected",
                    control_actor_from_params(params),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("not_owner_or_not_authorized"),
                );
                return;
            }
        }
        match inject_state.revoke_pending_user_message(message_id).await {
            harn_vm::bridge::PendingUserMessageMutationResult::Mutated => {
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "revoked",
                    "accepted",
                    actor.clone(),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    None,
                );
                self.send_response(
                    id,
                    serde_json::json!({"messageId": message_id, "status": "revoked"}),
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyRevoked => {
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "already_revoked",
                    "idempotent",
                    actor.clone(),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    None,
                );
                self.send_response(
                    id,
                    serde_json::json!({"messageId": message_id, "status": "already_revoked"}),
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyDelivered => {
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "already_delivered",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("already_delivered"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "already_delivered",
                    "pending inject already delivered",
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::UnknownMessageId => {
                self.emit_control_outcome(
                    &session_id,
                    "session/revoke_inject",
                    "unknown_message_id",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("unknown_message_id"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "unknown_message_id",
                    "unknown pending inject messageId",
                );
            }
        }
    }

    pub(super) async fn handle_session_replace_inject(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "session/replace_inject requires sessionId");
            return;
        };
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/replace_inject", false)
        else {
            return;
        };
        let Some(message_id) =
            self.pending_inject_message_id_param(id, params, "session/replace_inject")
        else {
            return;
        };
        let actor = control_actor_from_params(params);
        if let Some(owner) = self
            .inject_controls
            .get(&session_id)
            .and_then(|records| records.get(message_id))
            .map(|record| record.owner.clone())
        {
            if owner != actor && !actor_is_host_owner(&actor) {
                self.send_pending_inject_error_with_data(
                    id,
                    message_id,
                    "not_owner_or_not_authorized",
                    "pending inject is owned by another ACP actor",
                    serde_json::json!({
                        "actor": actor,
                        "owner": owner,
                    }),
                );
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "rejected",
                    "rejected",
                    control_actor_from_params(params),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("not_owner_or_not_authorized"),
                );
                return;
            }
        }
        let (content, transcript_content) =
            match normalize_session_inject_content("session/replace_inject", params) {
                Ok(content) => content,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    self.emit_control_outcome(
                        &session_id,
                        "session/replace_inject",
                        "rejected",
                        "rejected",
                        actor,
                        serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                        Some("invalid_content"),
                    );
                    return;
                }
            };
        match inject_state
            .replace_pending_user_message(message_id, content, transcript_content)
            .await
        {
            harn_vm::bridge::PendingUserMessageMutationResult::Mutated => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "replaced",
                    "accepted",
                    actor.clone(),
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    None,
                );
                self.send_response(
                    id,
                    serde_json::json!({ "messageId": message_id, "status": "replaced" }),
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyRevoked => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "already_revoked",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("already_revoked"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "already_revoked",
                    "pending inject already revoked",
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::AlreadyDelivered => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "already_delivered",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("already_delivered"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "already_delivered",
                    "pending inject already delivered",
                );
            }
            harn_vm::bridge::PendingUserMessageMutationResult::UnknownMessageId => {
                self.emit_control_outcome(
                    &session_id,
                    "session/replace_inject",
                    "unknown_message_id",
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id, "messageId": message_id}),
                    Some("unknown_message_id"),
                );
                self.send_pending_inject_error(
                    id,
                    message_id,
                    "unknown_message_id",
                    "unknown pending inject messageId",
                );
            }
        }
    }

    pub(super) fn session_inject_state(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &str,
        require_active_prompt: bool,
    ) -> Option<harn_vm::bridge::HostBridgeInjectionState> {
        let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return None;
        };
        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return None;
        };
        if require_active_prompt && session.host_bridge.is_none() {
            self.send_error(
                id,
                -32004,
                &format!("Session has no active prompt: {session_id}"),
            );
            return None;
        }
        Some(session.inject_state.clone())
    }

    pub(super) fn pending_inject_message_id_param<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let Some(message_id) = params.get("messageId").and_then(|v| v.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires messageId"));
            return None;
        };
        if message_id.trim().is_empty() {
            self.send_error(
                id,
                -32602,
                &format!("{method} requires non-empty messageId"),
            );
            return None;
        }
        Some(message_id)
    }

    pub(super) fn pending_reminder_id_param<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let Some(reminder_id) = params
            .get("reminderId")
            .or_else(|| params.get("reminder_id"))
            .and_then(|v| v.as_str())
        else {
            self.send_error(id, -32602, &format!("{method} requires reminderId"));
            return None;
        };
        if reminder_id.trim().is_empty() {
            self.send_error(
                id,
                -32602,
                &format!("{method} requires non-empty reminderId"),
            );
            return None;
        }
        Some(reminder_id)
    }

    pub(super) fn send_pending_inject_error(
        &self,
        id: &serde_json::Value,
        message_id: &str,
        reason: &str,
        message: &str,
    ) {
        self.send_pending_inject_error_with_data(
            id,
            message_id,
            reason,
            message,
            serde_json::Value::Null,
        );
    }

    pub(super) fn send_pending_inject_error_with_data(
        &self,
        id: &serde_json::Value,
        message_id: &str,
        reason: &str,
        message: &str,
        extra: serde_json::Value,
    ) {
        let mut data = serde_json::Map::new();
        data.insert("reason".to_string(), serde_json::json!(reason));
        data.insert("messageId".to_string(), serde_json::json!(message_id));
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                data.insert(key.clone(), value.clone());
            }
        }
        self.send_error_with_data(id, -32602, message, serde_json::Value::Object(data));
    }

    pub(super) fn send_pending_reminder_error(
        &self,
        id: &serde_json::Value,
        reminder_id: &str,
        reason: &str,
        message: &str,
    ) {
        self.send_error_with_data(
            id,
            -32602,
            message,
            serde_json::json!({
                "reason": reason,
                "reminderId": reminder_id,
            }),
        );
    }

    pub(super) async fn handle_session_remind(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            if !id.is_null() {
                self.send_error(id, -32602, "session/remind requires sessionId");
            }
            return;
        };
        let Some(session) = self.sessions.get(session_id) else {
            if !id.is_null() {
                self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            }
            return;
        };
        let Some(bridge) = session.host_bridge.clone() else {
            if !id.is_null() {
                self.send_error(
                    id,
                    -32004,
                    &format!("Session has no active bridge: {session_id}"),
                );
            }
            return;
        };
        match bridge.push_queued_session_remind_from_params(params).await {
            Ok(reminder_id) => {
                if !id.is_null() {
                    self.send_response(id, serde_json::json!({"reminderId": reminder_id}));
                }
            }
            Err(error) => {
                if !id.is_null() {
                    self.send_error(id, -32602, &format!("session/remind: {error}"));
                }
            }
        }
    }

    pub(super) async fn handle_session_pending_injections(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/pending_injections", false)
        else {
            return;
        };
        self.send_response(id, inject_state.pending_injections_json().await);
    }

    pub(super) async fn handle_session_revoke_reminder(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(inject_state) =
            self.session_inject_state(id, params, "session/revoke_reminder", false)
        else {
            return;
        };
        let Some(reminder_id) =
            self.pending_reminder_id_param(id, params, "session/revoke_reminder")
        else {
            return;
        };
        match inject_state.revoke_pending_reminder(reminder_id).await {
            harn_vm::bridge::PendingReminderMutationResult::Mutated => {
                self.send_response(
                    id,
                    serde_json::json!({"reminderId": reminder_id, "status": "revoked"}),
                );
            }
            harn_vm::bridge::PendingReminderMutationResult::AlreadyRevoked => {
                self.send_response(
                    id,
                    serde_json::json!({"reminderId": reminder_id, "status": "already_revoked"}),
                );
            }
            harn_vm::bridge::PendingReminderMutationResult::AlreadyDelivered => {
                self.send_pending_reminder_error(
                    id,
                    reminder_id,
                    "already_delivered",
                    "pending reminder already delivered",
                );
            }
            harn_vm::bridge::PendingReminderMutationResult::UnknownReminderId => {
                self.send_pending_reminder_error(
                    id,
                    reminder_id,
                    "unknown_reminder_id",
                    "unknown pending reminderId",
                );
            }
        }
    }
}
