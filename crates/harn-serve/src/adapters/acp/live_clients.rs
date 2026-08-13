use super::*;

pub(super) struct LiveClientOperation {
    pub(super) result: serde_json::Value,
    action: Option<&'static str>,
}

impl AcpServer {
    pub(super) fn handle_session_live_clients(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        self.handle_live_client_operation(id, params, "session/live_clients");
    }

    pub(super) fn handle_session_attach(&self, id: &serde_json::Value, params: &serde_json::Value) {
        self.handle_live_client_operation(id, params, "session/attach");
    }

    pub(super) fn handle_session_takeover(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        self.handle_live_client_operation(id, params, "session/takeover");
    }

    pub(super) fn handle_session_detach(&self, id: &serde_json::Value, params: &serde_json::Value) {
        self.handle_live_client_operation(id, params, "session/detach");
    }

    pub(super) fn handle_session_heartbeat(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        self.handle_live_client_operation(id, params, "session/heartbeat");
    }

    fn handle_live_client_operation(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        method: &'static str,
    ) {
        let Some(session_id) = live_session_id(self, id, params, method) else {
            return;
        };
        match apply_live_client_operation(method, &session_id, params) {
            Ok(operation) => write_live_client_operation(&self.output, id, &session_id, operation),
            Err(message) => self.send_error(id, -32602, &message),
        }
    }
}

pub(super) fn is_live_client_method(method: &str) -> bool {
    matches!(
        method,
        "session/live_clients"
            | "session/attach"
            | "session/takeover"
            | "session/detach"
            | "session/heartbeat"
    )
}

pub(super) fn apply_live_client_operation(
    method: &str,
    session_id: &str,
    params: &serde_json::Value,
) -> Result<LiveClientOperation, String> {
    match method {
        "session/live_clients" => {
            let clients = harn_vm::agent_sessions::live_clients(session_id)
                .ok_or_else(|| format!("Unknown session: {session_id}"))?
                .iter()
                .map(harn_vm::agent_sessions::live_client_json)
                .collect::<Vec<_>>();
            Ok(LiveClientOperation {
                result: serde_json::json!({ "object": "list", "data": clients }),
                action: None,
            })
        }
        "session/attach" => {
            let client_id = live_client_id(params, method)?;
            let mode = live_client_mode(params)?;
            let controller = mode == harn_vm::agent_sessions::LiveClientMode::Controller;
            let request = harn_vm::agent_sessions::AttachLiveClient {
                client_id,
                mode,
                takeover: params
                    .get("takeover")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                prompt_injection: params
                    .get("promptInjection")
                    .or_else(|| params.get("prompt_injection"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(controller),
                permission_routing: params
                    .get("permissionRouting")
                    .or_else(|| params.get("permission_routing"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(controller),
                metadata: live_client_metadata(params),
            };
            change_operation(
                "attached",
                harn_vm::agent_sessions::attach_live_client(session_id, request),
            )
        }
        "session/takeover" => change_operation(
            "takeover",
            harn_vm::agent_sessions::takeover_live_client(
                session_id,
                live_client_id(params, method)?,
                live_client_metadata(params),
            ),
        ),
        "session/detach" => change_operation(
            "detached",
            harn_vm::agent_sessions::detach_live_client(
                session_id,
                live_client_id(params, method)?,
                params
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                live_client_metadata(params),
            ),
        ),
        "session/heartbeat" => change_operation(
            "heartbeat",
            harn_vm::agent_sessions::heartbeat_live_client(
                session_id,
                live_client_id(params, method)?,
                live_client_metadata(params),
            ),
        ),
        _ => Err(format!("Unsupported live-client method: {method}")),
    }
}

pub(super) fn write_live_client_operation(
    output: &AcpOutput,
    id: &serde_json::Value,
    session_id: &str,
    operation: LiveClientOperation,
) {
    if let Some(action) = operation.action {
        let notification = harn_vm::jsonrpc::notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "live_session_client",
                    "_meta": {
                        "harn": {
                            "action": action,
                            "state": operation.result,
                        }
                    }
                }
            }),
        );
        if let Ok(line) = serde_json::to_string(&notification) {
            output.write_line(&line);
        }
    }
    let response = harn_vm::jsonrpc::response(id.clone(), operation.result);
    if let Ok(line) = serde_json::to_string(&response) {
        output.write_line(&line);
    }
}

fn change_operation(
    action: &'static str,
    change: Result<harn_vm::agent_sessions::LiveClientChange, String>,
) -> Result<LiveClientOperation, String> {
    change.map(|change| LiveClientOperation {
        result: harn_vm::agent_sessions::live_client_change_json(&change),
        action: Some(action),
    })
}

fn live_session_id(
    server: &AcpServer,
    id: &serde_json::Value,
    params: &serde_json::Value,
    method: &str,
) -> Option<String> {
    let Some(session_id) = session_id_param(params) else {
        server.send_error(id, -32602, &format!("{method} requires sessionId"));
        return None;
    };
    if !server.sessions.contains_key(&session_id) || !harn_vm::agent_sessions::exists(&session_id) {
        server.send_error(id, -32602, &format!("Unknown session: {session_id}"));
        return None;
    }
    Some(session_id)
}

fn live_client_id(params: &serde_json::Value, method: &str) -> Result<String, String> {
    params
        .get("clientId")
        .or_else(|| params.get("client_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{method} requires clientId"))
}

fn live_client_mode(
    params: &serde_json::Value,
) -> Result<harn_vm::agent_sessions::LiveClientMode, String> {
    match params
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("observer")
    {
        "observer" => Ok(harn_vm::agent_sessions::LiveClientMode::Observer),
        "controller" => Ok(harn_vm::agent_sessions::LiveClientMode::Controller),
        other => Err(format!(
            "session/attach: unsupported mode `{other}`; expected `observer` or `controller`"
        )),
    }
}

fn live_client_metadata(params: &serde_json::Value) -> serde_json::Value {
    params
        .get("metadata")
        .or_else(|| params.pointer("/_meta/harn"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}
