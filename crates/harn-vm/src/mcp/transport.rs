use super::*;

const MCP_AUTH_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);

/// Resolve a request embedded in a stable `input_required` result.
pub(crate) async fn resolve_embedded_input_request(
    server_name: &str,
    msg: &serde_json::Value,
    fixtures: Option<&crate::harness::CapabilityFixtureState>,
) -> Option<serde_json::Value> {
    let method = msg.get("method").and_then(|value| value.as_str())?;
    if method == crate::mcp_elicit::ELICITATION_METHOD {
        return Some(
            crate::mcp_elicit::dispatch_inbound_elicitation(server_name, msg, fixtures).await,
        );
    }
    if method == crate::mcp_sampling::SAMPLING_METHOD {
        return Some(crate::mcp_sampling::dispatch_inbound_sampling(server_name, msg).await);
    }
    if method == crate::mcp_protocol::METHOD_ROOTS_LIST {
        let id = msg.get("id")?.clone();
        return Some(harn_roots_list_response(id));
    }
    unsupported_embedded_input_response(msg)
}

fn relay_stream_notification(server_name: &str, msg: &serde_json::Value) -> bool {
    let Some(method) = msg.get("method").and_then(|value| value.as_str()) else {
        return false;
    };
    if method == "notifications/progress" {
        relay_progress_notification(server_name, msg);
        return true;
    }
    if method == "notifications/message" {
        relay_log_notification(server_name, msg);
        return true;
    }
    if method == "notifications/resources/updated"
        || method == "notifications/resources/list_changed"
        || method == "notifications/tools/list_changed"
        || method == "notifications/prompts/list_changed"
    {
        relay_resource_notification(server_name, method, msg);
        return true;
    }
    false
}

pub(crate) async fn http_call_raw(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let id = inner.next_id;
    inner.next_id += 1;
    send_http_request(inner, server_name, method, params, Some(id)).await
}

pub(crate) async fn http_notify(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<(), VmError> {
    let _ = send_http_request(inner, server_name, method, params, None).await?;
    Ok(())
}

pub(crate) async fn send_http_request(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
    id: Option<u64>,
) -> Result<serde_json::Value, VmError> {
    let mut auth_retry_used = false;
    loop {
        let auth_completion_rx = crate::mcp_oauth::subscribe_authorization_completions();
        let response = send_http_request_once(inner, method, params.clone(), id).await?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if let Some(protocol_version) = headers
            .get(MCP_HEADER_PROTOCOL_VERSION)
            .and_then(|v| v.to_str().ok())
        {
            inner.protocol_version = protocol_version.to_string();
        }
        // RFC 6750 §3.1: a `401` means no/invalid/expired token; a `403` with
        // `error="insufficient_scope"` means the token is valid but lacks a
        // required scope. Both carry a Bearer `WWW-Authenticate` challenge and
        // both are resolved by re-running the OAuth flow — the emitted
        // `mcp_auth_required` event carries the challenge's `scope`, so a
        // step-up authorization requests exactly the elevated scope. A plain
        // `403` without an `insufficient_scope` challenge is a genuine denial,
        // not an authorization gap, so it falls through unchanged.
        if status == 401 || (status == 403 && www_authenticate_insufficient_scope(&headers)) {
            emit_mcp_auth_required_event(server_name, &inner.url, &headers);
            if auth_retry_used {
                return Err(mcp_auth_required_error(
                    server_name,
                    &inner.url,
                    "server still returned an authorization challenge after authorization completed",
                ));
            }
            auth_retry_used = true;
            wait_for_http_mcp_authorization(inner, server_name, auth_completion_rx).await?;
            continue;
        }

        let body = response.text().await.map_err(|e| {
            VmError::Runtime(format!(
                "MCP HTTP read error: {}",
                crate::egress::redact_reqwest_error(&e)
            ))
        })?;

        if body.trim().is_empty() {
            if status >= 400 {
                return Err(VmError::Runtime(format!(
                    "MCP HTTP request returned {status} with an empty response body"
                )));
            }
            return Ok(serde_json::Value::Null);
        }

        let msg = parse_http_response_body(server_name, &body, status, id)?;

        if status >= 400 && id.is_none() {
            return Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)));
        }
        return Ok(msg);
    }
}

/// True when any `WWW-Authenticate` Bearer challenge in the response headers
/// carries `error="insufficient_scope"` (RFC 6750 §3.1). Paired with a `403`
/// status, this is the cue to run a step-up authorization for the elevated
/// scope rather than treating the response as a hard denial.
fn www_authenticate_insufficient_scope(headers: &reqwest::header::HeaderMap) -> bool {
    let challenges: Vec<&str> = headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    crate::mcp_auth::parse_www_authenticate_headers(challenges.iter().copied())
        .iter()
        .any(crate::mcp_auth::WwwAuthenticateChallenge::is_insufficient_scope)
}

async fn wait_for_http_mcp_authorization(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    auth_completion_rx: tokio::sync::broadcast::Receiver<crate::mcp_oauth::StoredMcpToken>,
) -> Result<(), VmError> {
    if crate::llm::current_agent_session_id().is_none() {
        return Err(mcp_auth_required_error(
            server_name,
            &inner.url,
            "no active agent session is available to surface an authorization prompt",
        ));
    }
    if crate::llm::current_host_bridge().is_none() {
        return Err(mcp_auth_required_error(
            server_name,
            &inner.url,
            "no interactive host is available to complete OAuth",
        ));
    }

    let resource = crate::mcp_auth::canonical_resource_indicator(&inner.url)
        .unwrap_or_else(|_| inner.url.clone());
    let token = crate::mcp_oauth::wait_for_authorization_completion(
        &resource,
        MCP_AUTH_COMPLETION_TIMEOUT,
        auth_completion_rx,
    )
    .await
    .map_err(|error| mcp_auth_required_error(server_name, &inner.url, &error))?;
    inner.auth_token = Some(token.access_token);
    inner.auth_token_source = HttpAuthTokenSource::OAuthStore;
    Ok(())
}

fn mcp_auth_required_error(server_name: &str, server_url: &str, reason: &str) -> VmError {
    let resource = crate::mcp_auth::canonical_resource_indicator(server_url)
        .unwrap_or_else(|_| server_url.to_string());
    VmError::CategorizedError {
        category: crate::value::ErrorCategory::Auth,
        message: format!("MCP authorization required for {server_name} ({resource}): {reason}"),
    }
}

pub(crate) async fn send_http_request_once(
    inner: &mut HttpMcpClientInner,
    method: &str,
    params: serde_json::Value,
    id: Option<u64>,
) -> Result<reqwest::Response, VmError> {
    let request_params = request_params_for_protocol(&inner.protocol_version, params);
    let mut payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": request_params,
    });
    if let Some(id) = id {
        payload["id"] = serde_json::json!(id);
    }
    let payload = wrap_http_payload(payload, inner.proxy_server_name.as_deref());
    let auth_token = resolve_http_request_auth_token(inner).await?;

    let request = inner
        .client
        .post(&inner.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&payload);
    let request = apply_http_headers(
        request,
        &auth_token,
        &inner.protocol_version,
        Some(method),
        payload.get("params"),
        &inner.tool_headers,
    );

    request.timeout(MCP_TIMEOUT).send().await.map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP request error: {}",
            crate::egress::redact_reqwest_error(&e)
        ))
    })
}

async fn resolve_http_request_auth_token(
    inner: &mut HttpMcpClientInner,
) -> Result<Option<String>, VmError> {
    let Some(base_token) = inner.auth_token.clone() else {
        return Ok(None);
    };
    let Some(config) = inner
        .token_exchange
        .clone()
        .filter(|config| config.is_enabled())
    else {
        return Ok(Some(base_token));
    };
    let Some(actor_chain) = crate::agent_sessions::current_actor_chain() else {
        return Ok(Some(base_token));
    };
    if !actor_chain.is_delegated() {
        return Ok(Some(base_token));
    }

    match inner.auth_token_source {
        HttpAuthTokenSource::OAuthStore => {
            match crate::mcp_oauth::resolve_delegated_bearer_from_store(
                &inner.url,
                &config,
                &actor_chain,
            )
            .await
            .map_err(|error| VmError::Runtime(format!("MCP token exchange failed: {error}")))?
            {
                Some(resolved) => {
                    inner.auth_token = Some(resolved.base_bearer);
                    Ok(Some(resolved.bearer))
                }
                None => Ok(Some(base_token)),
            }
        }
        HttpAuthTokenSource::Config => {
            let exchanged = crate::mcp_oauth::exchange_configured_bearer_for_actor_chain(
                &inner.url,
                &base_token,
                &config,
                &actor_chain,
            )
            .await
            .map_err(|error| VmError::Runtime(format!("MCP token exchange failed: {error}")))?;
            Ok(exchanged.or(Some(base_token)))
        }
        HttpAuthTokenSource::None => Ok(None),
    }
}

pub(crate) fn apply_http_headers(
    mut request: reqwest::RequestBuilder,
    auth_token: &Option<String>,
    protocol_version: &str,
    method: Option<&str>,
    params: Option<&serde_json::Value>,
    tool_headers: &BTreeMap<String, Vec<McpToolHeader>>,
) -> reqwest::RequestBuilder {
    request = request.header(MCP_HEADER_PROTOCOL_VERSION, protocol_version);
    if let Some(token) = auth_token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(method) = method {
        request = request.header(MCP_HEADER_METHOD, method);
        if let Some(params) = params {
            if let Some(name) = standard_name_header_value(method, params) {
                request = request.header(MCP_HEADER_NAME, name);
            }
        }
        if method == "tools/call" {
            request = apply_mcp_tool_parameter_headers(request, params, tool_headers);
        }
    }
    request
}

pub(crate) fn apply_mcp_tool_parameter_headers(
    mut request: reqwest::RequestBuilder,
    params: Option<&serde_json::Value>,
    tool_headers: &BTreeMap<String, Vec<McpToolHeader>>,
) -> reqwest::RequestBuilder {
    let Some(params) = params else {
        return request;
    };
    let Some(tool_name) = params.get("name").and_then(|value| value.as_str()) else {
        return request;
    };
    let Some(headers) = tool_headers.get(tool_name) else {
        return request;
    };
    let Some(arguments) = params.get("arguments").and_then(|value| value.as_object()) else {
        return request;
    };

    for header in headers {
        let Some(value) = arguments.get(&header.parameter) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let Some(encoded) = encode_mcp_header_value(value) else {
            continue;
        };
        request = request.header(header.header_name.as_str(), encoded);
    }
    request
}

pub(crate) fn wrap_http_payload(
    payload: serde_json::Value,
    proxy_server_name: Option<&str>,
) -> serde_json::Value {
    let Some(proxy_server_name) = proxy_server_name else {
        return payload;
    };
    let mut wrapped = serde_json::Map::new();
    wrapped.insert(
        "serverName".to_string(),
        serde_json::Value::String(proxy_server_name.to_string()),
    );
    if let Some(object) = payload.as_object() {
        for (key, value) in object {
            wrapped.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(wrapped)
}

pub(crate) fn parse_http_response_body(
    server_name: &str,
    body: &str,
    status: u16,
    request_id: Option<u64>,
) -> Result<serde_json::Value, VmError> {
    if body.trim_start().starts_with("event:") || body.trim_start().starts_with("data:") {
        return parse_sse_jsonrpc_body(server_name, body, request_id);
    }
    parse_jsonrpc_message(body.as_bytes()).map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP response parse error (status {status}): {e}"
        ))
    })
}

pub(crate) fn parse_sse_jsonrpc_body(
    server_name: &str,
    body: &str,
    request_id: Option<u64>,
) -> Result<serde_json::Value, VmError> {
    let mut current_data = Vec::new();
    let mut messages = Vec::new();

    for line in body.lines() {
        if line.is_empty() {
            if !current_data.is_empty() {
                messages.push(current_data.join("\n"));
                current_data.clear();
            }
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            current_data.push(data.trim_start().to_string());
        }
    }
    if !current_data.is_empty() {
        messages.push(current_data.join("\n"));
    }

    let mut fallback = None;
    for message in messages {
        if let Ok(value) = parse_jsonrpc_message(message.as_bytes()) {
            if request_id.is_some()
                && value["id"].as_u64() == request_id
                && (value.get("result").is_some() || value.get("error").is_some())
            {
                return Ok(value);
            }
            if relay_stream_notification(server_name, &value) {
                continue;
            }
            if value.get("result").is_some() || value.get("error").is_some() {
                fallback = Some(value);
            }
        }
    }

    fallback.ok_or_else(|| {
        VmError::Runtime(
            "MCP HTTP response parse error: no JSON-RPC payload found in SSE stream".into(),
        )
    })
}
