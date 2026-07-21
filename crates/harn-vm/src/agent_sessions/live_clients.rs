use super::*;

pub fn live_clients(id: &str) -> Option<Vec<LiveSessionClient>> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.live_clients.values().cloned().collect())
    })
}

pub fn attach_live_client(id: &str, request: AttachLiveClient) -> Result<LiveClientChange, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let client_id = validate_live_client_id(request.client_id)?;
        let now = crate::orchestration::now_unix_seconds_text();
        let previous_clients = state.live_clients.clone();
        let previous_controller_id = state.live_controller_id.clone();

        if request.mode == LiveClientMode::Controller {
            let conflicting_controller = previous_controller_id
                .as_ref()
                .filter(|controller_id| *controller_id != &client_id)
                .filter(|controller_id| state.live_clients.contains_key(*controller_id));
            if let Some(previous) = conflicting_controller {
                if !request.takeover {
                    return Err(format!("live session already has controller '{previous}'"));
                }
                if let Some(previous_client) = state.live_clients.get_mut(previous) {
                    previous_client.mode = LiveClientMode::Observer;
                    previous_client.prompt_injection = false;
                    previous_client.permission_routing = false;
                    previous_client.last_seen_at = now.clone();
                }
            }
            state.live_controller_id = Some(client_id.clone());
        } else if state.live_controller_id.as_deref() == Some(client_id.as_str()) {
            state.live_controller_id = None;
        }

        let attached_at = state
            .live_clients
            .get(&client_id)
            .map(|client| client.attached_at.clone())
            .unwrap_or_else(|| now.clone());
        let client = LiveSessionClient {
            client_id: client_id.clone(),
            mode: request.mode,
            attached_at,
            last_seen_at: now,
            prompt_injection: request.prompt_injection,
            permission_routing: request.permission_routing,
            metadata: request.metadata,
        };
        state.live_clients.insert(client_id, client.clone());
        state.touch();
        let active_controller_id = state.live_controller_id.clone();
        append_live_client_event(
            state,
            "attached",
            Some(&client),
            previous_controller_id.as_deref(),
            active_controller_id.as_deref(),
            serde_json::Value::Null,
        )
        .inspect_err(|_error| {
            state.live_clients = previous_clients;
            state.live_controller_id = previous_controller_id.clone();
        })?;
        Ok(live_client_change(
            Some(client),
            previous_controller_id,
            state,
        ))
    })
}

pub fn takeover_live_client(
    id: &str,
    client_id: impl Into<String>,
    metadata: serde_json::Value,
) -> Result<LiveClientChange, String> {
    attach_live_client(
        id,
        AttachLiveClient {
            client_id: client_id.into(),
            mode: LiveClientMode::Controller,
            takeover: true,
            prompt_injection: true,
            permission_routing: true,
            metadata,
        },
    )
}

pub fn detach_live_client(
    id: &str,
    client_id: impl Into<String>,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> Result<LiveClientChange, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let client_id = validate_live_client_id(client_id.into())?;
        let previous_clients = state.live_clients.clone();
        let previous_controller_id = state.live_controller_id.clone();
        let Some(mut client) = state.live_clients.remove(&client_id) else {
            return Err(format!("live client '{client_id}' is not attached"));
        };
        client.last_seen_at = crate::orchestration::now_unix_seconds_text();
        if state.live_controller_id.as_deref() == Some(client_id.as_str()) {
            state.live_controller_id = None;
        }
        state.touch();
        let active_controller_id = state.live_controller_id.clone();
        append_live_client_event(
            state,
            "detached",
            Some(&client),
            previous_controller_id.as_deref(),
            active_controller_id.as_deref(),
            serde_json::json!({
                "reason": reason.unwrap_or_else(|| "client_detached".to_string()),
                "metadata": metadata,
            }),
        )
        .inspect_err(|_error| {
            state.live_clients = previous_clients;
            state.live_controller_id = previous_controller_id.clone();
        })?;
        Ok(live_client_change(None, previous_controller_id, state))
    })
}

pub fn heartbeat_live_client(
    id: &str,
    client_id: impl Into<String>,
    metadata: serde_json::Value,
) -> Result<LiveClientChange, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let client_id = validate_live_client_id(client_id.into())?;
        let previous_clients = state.live_clients.clone();
        let previous_controller_id = state.live_controller_id.clone();
        let Some(client) = state.live_clients.get_mut(&client_id) else {
            return Err(format!("live client '{client_id}' is not attached"));
        };
        client.last_seen_at = crate::orchestration::now_unix_seconds_text();
        if !metadata.is_null() {
            client.metadata = metadata.clone();
        }
        let client = client.clone();
        state.touch();
        let active_controller_id = state.live_controller_id.clone();
        append_live_client_event(
            state,
            "heartbeat",
            Some(&client),
            previous_controller_id.as_deref(),
            active_controller_id.as_deref(),
            serde_json::json!({ "metadata": metadata }),
        )
        .inspect_err(|_error| {
            state.live_clients = previous_clients;
            state.live_controller_id = previous_controller_id.clone();
        })?;
        Ok(live_client_change(
            Some(client),
            previous_controller_id,
            state,
        ))
    })
}

pub fn inject_prompt_from_live_client(
    id: &str,
    client_id: impl Into<String>,
    content: VmValue,
    metadata: serde_json::Value,
) -> Result<(), String> {
    let client_id = validate_live_client_id(client_id.into())?;
    ensure_live_controller(id, &client_id, LiveControllerCapability::PromptInjection)?;
    let mut message = BTreeMap::new();
    message.put_str("role", "user");
    message.insert("content".to_string(), content);
    message.insert(
        "metadata".to_string(),
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "live_session": {
                "client_id": client_id,
                "mode": "controller",
                "source": "live_session_attach",
                "metadata": metadata,
            }
        })),
    );
    inject_message(id, VmValue::dict(message))
}

pub fn route_live_permission_request(
    id: &str,
    client_id: impl Into<String>,
    request: serde_json::Value,
    metadata: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let client_id = validate_live_client_id(client_id.into())?;
    let client =
        ensure_live_controller(id, &client_id, LiveControllerCapability::PermissionRouting)?;
    let request_id = request
        .get("id")
        .or_else(|| request.get("request_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or("permission_request");
    let event_metadata = serde_json::json!({
        "action": "permission_routed",
        "client": live_client_json(&client),
        "request_id": request_id,
        "request": request,
        "metadata": metadata,
    });
    let event = crate::llm::helpers::transcript_event(
        LIVE_CLIENT_PERMISSION_EVENT_KIND,
        "system",
        "internal",
        "Live session permission request routed",
        Some(event_metadata.clone()),
    );
    append_event(id, event)?;
    Ok(event_metadata)
}

enum LiveControllerCapability {
    PromptInjection,
    PermissionRouting,
}

fn ensure_live_controller(
    id: &str,
    client_id: &str,
    capability: LiveControllerCapability,
) -> Result<LiveSessionClient, String> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        if state.live_controller_id.as_deref() != Some(client_id) {
            return Err(format!(
                "live client '{client_id}' is not the active controller"
            ));
        }
        let Some(client) = state.live_clients.get(client_id) else {
            return Err(format!("live client '{client_id}' is not attached"));
        };
        match capability {
            LiveControllerCapability::PromptInjection if !client.prompt_injection => Err(format!(
                "live client '{client_id}' cannot inject prompts for this session"
            )),
            LiveControllerCapability::PermissionRouting if !client.permission_routing => Err(
                format!("live client '{client_id}' cannot route permissions for this session"),
            ),
            _ => Ok(client.clone()),
        }
    })
}

fn append_live_client_event(
    state: &mut SessionState,
    action: &str,
    client: Option<&LiveSessionClient>,
    previous_controller_id: Option<&str>,
    active_controller_id: Option<&str>,
    extra: serde_json::Value,
) -> Result<(), String> {
    let metadata = serde_json::json!({
        "action": action,
        "session_id": state.id,
        "client": client.map(live_client_json),
        "previous_controller_id": previous_controller_id,
        "active_controller_id": active_controller_id,
        "clients": state
            .live_clients
            .values()
            .map(live_client_json)
            .collect::<Vec<_>>(),
        "extra": extra,
    });
    let event = crate::llm::helpers::transcript_event(
        LIVE_CLIENT_EVENT_KIND,
        "system",
        "internal",
        "Live session client lifecycle changed",
        Some(metadata),
    );
    append_event_to_state(state, event, "live_client")
}

fn live_client_change(
    client: Option<LiveSessionClient>,
    previous_controller_id: Option<String>,
    state: &SessionState,
) -> LiveClientChange {
    LiveClientChange {
        client,
        previous_controller_id,
        active_controller_id: state.live_controller_id.clone(),
        clients: state.live_clients.values().cloned().collect(),
    }
}

fn validate_live_client_id(id: impl Into<String>) -> Result<String, String> {
    let id = id.into();
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err("live client id cannot be empty".to_string());
    }
    Ok(trimmed.to_string())
}

pub fn live_client_json(client: &LiveSessionClient) -> serde_json::Value {
    serde_json::json!({
        "client_id": client.client_id,
        "mode": client.mode.as_str(),
        "attached_at": client.attached_at,
        "last_seen_at": client.last_seen_at,
        "prompt_injection": client.prompt_injection,
        "permission_routing": client.permission_routing,
        "metadata": client.metadata,
    })
}

pub fn live_client_change_json(change: &LiveClientChange) -> serde_json::Value {
    serde_json::json!({
        "client": change.client.as_ref().map(live_client_json),
        "previous_controller_id": change.previous_controller_id,
        "active_controller_id": change.active_controller_id,
        "clients": change
            .clients
            .iter()
            .map(live_client_json)
            .collect::<Vec<_>>(),
    })
}
