use super::*;

/// Forward an MCP `notifications/progress` event into the originating
/// agent session's inbox. Correlation by `progressToken`: every
/// outgoing `tools/call` issued by [`call_mcp_tool`] registers a fresh
/// token in [`client_progress`]; this function looks up the inbound
/// notification's token and routes it back to whichever session is
/// bound to it. Falls back to the thread-local current session for
/// notifications without a token (e.g. servers that emit progress
/// unsolicited). Drops silently when neither a token mapping nor a
/// current session is present.
pub(crate) fn relay_progress_notification(server_name: &str, msg: &serde_json::Value) {
    let params = msg.get("params");
    let progress_token = params
        .and_then(|p| p.get("progressToken"))
        .and_then(|t| match t {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
    let token_context = progress_token.as_deref().and_then(client_progress::lookup);
    let session_id = token_context
        .as_ref()
        .and_then(|ctx| ctx.session_id.clone())
        .or_else(crate::llm::current_agent_session_id);
    let Some(session_id) = session_id else {
        return;
    };
    let mut payload = params.cloned().unwrap_or(serde_json::Value::Null);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "server".to_string(),
            serde_json::Value::String(server_name.to_string()),
        );
        if let Some(ctx) = token_context.as_ref() {
            obj.insert(
                "tool".to_string(),
                serde_json::Value::String(ctx.tool.clone()),
            );
        }
    } else {
        payload = serde_json::json!({
            "server": server_name,
            "tool": token_context.as_ref().map(|c| c.tool.as_str()).unwrap_or(""),
            "raw": payload,
        });
    }
    emit_mcp_notification_event(
        &session_id,
        server_name,
        "notifications/progress",
        "notification",
        &payload,
    );
    let content = serde_json::to_string(&payload).unwrap_or_default();
    crate::orchestration::agent_inbox::push(
        &session_id,
        "mcp_progress",
        &content,
        "mcp.notifications/progress",
    );
}

/// Emit an `AgentEvent::McpNotification` so observers (notably the ACP
/// adapter's `_harn/agentEvent` channel) can render the server-to-client
/// message live. This is purely additive — the inbox relay still runs so
/// the agent loop continues to see these messages as feedback.
pub(crate) fn emit_mcp_notification_event(
    session_id: &str,
    server_name: &str,
    method: &str,
    direction: &str,
    params: &serde_json::Value,
) {
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::McpNotification {
        session_id: session_id.to_string(),
        server: server_name.to_string(),
        method: method.to_string(),
        direction: direction.to_string(),
        params: params.clone(),
    });
}

/// Emit an `AgentEvent::McpAuthRequired` when a server answers with `401` so a
/// thin ACP client can start an interactive OAuth authorization. Token exchange
/// and storage stay in harn (see [`crate::mcp_oauth`]); this is purely the cue.
/// No-op outside an agent session (e.g. ad hoc CLI MCP calls).
pub(crate) fn emit_mcp_auth_required_event(
    server_name: &str,
    server_url: &str,
    headers: &reqwest::header::HeaderMap,
) {
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    let resource = crate::mcp_auth::canonical_resource_indicator(server_url)
        .unwrap_or_else(|_| server_url.to_string());
    let challenges: Vec<&str> = headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    let scope = crate::mcp_auth::bearer_challenge_from_headers(challenges.iter().copied())
        .and_then(|challenge| challenge.bearer_scope().map(str::to_string));
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::McpAuthRequired {
        session_id,
        server: server_name.to_string(),
        resource,
        scope,
    });
}

/// Emit an `AgentEvent::McpCatalogChanged` when a connected server announces
/// that its tool/resource/prompt list changed (`notifications/*/list_changed`).
/// A thin ACP client (an IDE host's TUI/GUI) treats this as a cue to re-fetch
/// the catalog so newly added tools become visible without a restart. No-op
/// outside an agent session.
pub(crate) fn emit_mcp_catalog_changed_event(server_name: &str, reason: &str) {
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::McpCatalogChanged {
        session_id,
        server: Some(server_name.to_string()),
        reason: reason.to_string(),
    });
}

pub(crate) fn relay_log_notification(server_name: &str, msg: &serde_json::Value) {
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    let mut payload = msg
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "server".to_string(),
            serde_json::Value::String(server_name.to_string()),
        );
    }
    emit_mcp_notification_event(
        &session_id,
        server_name,
        "notifications/message",
        "notification",
        &payload,
    );
    let content = serde_json::to_string(&payload).unwrap_or_default();
    crate::orchestration::agent_inbox::push(
        &session_id,
        "mcp_log",
        &content,
        "mcp.notifications/message",
    );
}

pub(crate) fn relay_resource_notification(
    server_name: &str,
    method: &str,
    msg: &serde_json::Value,
) {
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    let payload = serde_json::json!({
        "server": server_name,
        "method": method,
        "params": msg.get("params").cloned().unwrap_or(serde_json::Value::Null),
    });
    emit_mcp_notification_event(&session_id, server_name, method, "notification", &payload);
    // A `*/list_changed` notification means the server's advertised
    // tools/resources/prompts changed. Additionally emit the catalog-change cue
    // so a thin client re-fetches `mcp/catalog` and surfaces the new tools this
    // session — the `mcp_notification` relay above still runs so the agent loop
    // sees the raw message too. `resources/updated` is a content change, not a
    // catalog change, so it is intentionally excluded.
    if method.ends_with("/list_changed") {
        emit_mcp_catalog_changed_event(server_name, "list_changed");
    }
    let content = serde_json::to_string(&payload).unwrap_or_default();
    crate::orchestration::agent_inbox::push(
        &session_id,
        "mcp_resource_change",
        &content,
        "mcp.notifications",
    );
}
