//! Which requests an attached client may send, and who owns the ones in flight.

use super::*;

pub(super) struct AcpClientRequest {
    pub(super) client_id: String,
    pub(super) delivery: bool,
}

impl AcpWorker {
    pub(super) fn register_client_request(
        &self,
        client_id: &str,
        value: &JsonValue,
    ) -> Result<(), AcpClientRequestError> {
        if let Some(id_key) = jsonrpc_id_key(value) {
            if value.get("method").is_some() {
                let mut pending = self
                    .pending_client_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if pending.contains_key(&id_key) {
                    return Err(AcpClientRequestError::DuplicateRequestId);
                }
                pending.insert(
                    id_key,
                    AcpClientRequest {
                        client_id: client_id.to_string(),
                        delivery: value["method"] == "session/prompt"
                            && value
                                .pointer("/params/delivery")
                                .is_some_and(|value| !value.is_null()),
                    },
                );
            }
        }
        Ok(())
    }

    /// Withdraw queued delivery prompts when their client leaves. Ordinary
    /// turns remain live in the retained worker for reconnection and replay.
    pub(super) fn withdraw_client_requests(&self, client_id: &str) {
        let request_ids: Vec<String> = self
            .pending_client_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, request)| request.client_id == client_id && request.delivery)
            .map(|(request_id, _)| request_id.clone())
            .collect();
        let sender = self
            .request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned();
        let Some(sender) = sender else {
            return;
        };
        for request_id in request_ids {
            let Ok(request_id) = serde_json::from_str::<JsonValue>(&request_id) else {
                continue;
            };
            let _ = sender.send(json!({
                "jsonrpc": "2.0",
                "method": "$/cancel_request",
                "params": {"requestId": request_id},
            }));
        }
    }

    /// A client may withdraw only the requests it sent.
    pub(super) fn owns_client_request(&self, client_id: &str, cancel: &JsonValue) -> bool {
        let Some(request_id) = cancel
            .pointer("/params/requestId")
            .and_then(|id| serde_json::to_string(id).ok())
        else {
            return false;
        };
        self.pending_client_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&request_id)
            .is_some_and(|request| request.client_id == client_id)
    }
}

/// A controller steers or queues into the host owner's session; only the host
/// owner starts plain turns.
pub(super) fn role_may_send_method(role: AcpAttachRole, method: &str, request: &JsonValue) -> bool {
    match role {
        AcpAttachRole::HostOwner => true,
        AcpAttachRole::Controller => match method {
            "session/prompt" => request
                .pointer("/params/delivery")
                .is_some_and(|delivery| !delivery.is_null()),
            _ => matches!(
                method,
                "$/cancel_request"
                    | "session/cancel"
                    | "session/truncate"
                    | "session/remind"
                    | "session/pending_prompts"
                    | "session/revoke_reminder"
                    | "session/set_mode"
                    | "session/set_config_option"
            ),
        },
        AcpAttachRole::Observer => false,
    }
}
