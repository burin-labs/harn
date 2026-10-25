use super::sessions::{ToolCallCancelRequest, ToolCallCancelResult};
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
        prepared_cancellation: Option<Result<bool, TaskAdmissionError>>,
    ) {
        let Some(session_id) = session_id_param(params) else {
            if !id.is_null() {
                self.send_error(id, -32602, "session/cancel requires sessionId");
            }
            return;
        };
        let Some(_) = lookup_session_cancellation(&self.session_cancellations, &session_id) else {
            // A stop the server cannot honor must say so. The notification form
            // has no response to carry a refusal, and an unregistered session
            // has no event sink the control outcome is guaranteed to reach, so
            // the log is the one record that always survives: without it, a
            // cancel aimed at an unregistered session is indistinguishable from
            // one that worked, which is exactly how a dead stop control hides.
            tracing::warn!(
                session_id = %session_id,
                "session/cancel names a session this server has not registered; nothing was cancelled"
            );
            self.emit_control_outcome(
                &session_id,
                "session/cancel",
                "unknown_session",
                "rejected",
                control_actor_from_params(params),
                serde_json::json!({"sessionId": session_id}),
                Some("no live session is registered under this id"),
            );
            if !id.is_null() {
                self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            }
            return;
        };

        let actor = control_actor_from_params(params);
        let result = prepared_cancellation.unwrap_or_else(|| {
            prompt_owner::cancel_session_prompt(
                &self.session_cancellations,
                &self.concurrent_controls,
                params,
            )
            .expect("registered session")
        });
        let newly_cancelled = match result {
            Ok(cancelled) => cancelled,
            Err(error) => {
                prompt_owner::record_cancel_refusal(
                    &self.concurrent_controls,
                    &session_id,
                    params,
                    error,
                );
                if !id.is_null() {
                    self.send_error(id, error.rpc_code(), error.message());
                }
                return;
            }
        };
        let status = if newly_cancelled {
            "cancelled"
        } else {
            "already_cancelled"
        };
        // An accepted stop is a control event, not just a flag flip. The
        // completion judge and every forensic reader need it in the
        // session's own event stream; recovering it later by matching
        // stop-sounding prose misclassifies task prompts that happen to
        // contain the same phrase.
        self.record_and_emit_control(
            &session_id,
            harn_session_store::ControlEvent::stop("session/cancel", control_id(), status)
                .with_actor(actor.clone()),
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
        let request = match ToolCallCancelRequest::parse(params) {
            Ok(request) => request,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        let result = match self.sessions.get(&request.session_id) {
            Some(session) => request.cancel(&session.concurrent_control.tool_call_cancellations),
            None => ToolCallCancelResult::not_found(request.call_id),
        };
        self.send_response(id, result.into_value());
    }

    pub(super) async fn handle_session_close(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &str,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return;
        };

        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return;
        };

        // Stop admitting delivery prompts before sweeping the pending ones, so
        // a prompt racing this close is refused with an error. Held until the
        // close ends: on failure the session stays open and admits again.
        let _closed = self
            .concurrent_controls
            .pending_prompts
            .close_delivery(session_id);
        session.cancellation.cancel();
        self.concurrent_controls
            .cancel_pending_prompts(session_id, "session_closed", &self.output)
            .await;
        if let Err(error) = self.clear_active_prompt_transport(session_id).await {
            self.send_error(
                id,
                -32000,
                &format!("Failed to persist session {session_id} before close: {error}"),
            );
            return;
        }
        if let Err(error) = harn_vm::agent_sessions::close_with_status(
            session_id,
            "client_request",
            "closed",
            serde_json::json!({
                "protocol": "acp",
                "method": method,
            }),
        ) {
            self.send_error(
                id,
                -32000,
                &format!("Failed to close session {session_id}: {error}"),
            );
            return;
        }
        let closed = self
            .sessions
            .remove(session_id)
            .expect("validated session should still exist after sink flush");
        self.release_session_workspace_warm(&closed);
        self.session_cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
        self.concurrent_controls.remove(session_id);
        self.timeline_subscriptions.retain(|_, subscription| {
            if subscription.session_id.as_deref() == Some(session_id) {
                subscription.handle.abort();
                false
            } else {
                true
            }
        });
        #[cfg(feature = "hostlib")]
        {
            harn_hostlib::fs_snapshot::drop_session_snapshots(session_id);
            harn_hostlib::workspace_checkpoint::drop_session(session_id);
        }
        self.send_response(id, serde_json::json!({}));
    }

    pub(super) async fn clear_active_prompt_transport(
        &mut self,
        session_id: &str,
    ) -> Result<(), harn_vm::agent_events::AgentEventSinkError> {
        // Close admission before waiting for the queue. An admitted input either
        // becomes visible before this sweep or is rejected by the closed owner.
        let closing = self.sessions.get(session_id).and_then(|session| {
            session
                .concurrent_control
                .close_prompt_owner()
                .map(|owner| (owner, session.concurrent_control.inject_state.clone()))
        });
        if let Some((owner, inject_state)) = closing {
            if let Some(task) = owner.task() {
                for message in inject_state.revoke_task_user_messages(&task.task_id).await {
                    self.concurrent_controls.fail_pending_prompt(
                        session_id,
                        &message.message_id,
                        "task_prompt_ended",
                        &self.output,
                    );
                }
            }
        }
        let flush_result = flush_and_clear_session_sinks(session_id).await;
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.host_bridge = None;
            session.concurrent_control.set_prompt_active(false);
            session
                .concurrent_control
                .set_prompt_transport(harn_vm::agent_events::AgentEventTransport::default());
        }
        flush_result
    }

    pub(super) fn session_inject_state(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &str,
    ) -> Option<harn_vm::bridge::HostBridgeInjectionState> {
        let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return None;
        };
        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return None;
        };
        Some(session.inject_state.clone())
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

    pub(super) async fn handle_session_revoke_reminder(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(inject_state) = self.session_inject_state(id, params, "session/revoke_reminder")
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
