use super::*;

const MCP_AUTH_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);

/// Upper bound on a single line read from an MCP stdio server, and on the
/// messages buffered while a request write is in flight. `read_line` grows its
/// buffer without bound, so a misbehaving server streaming an endless line
/// could otherwise OOM the host.
const MCP_STDIO_MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) async fn stdio_call_raw(
    inner: &mut StdioMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    for _ in 0..2 {
        let id = inner.next_id;
        inner.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": request_params_for_protocol(
                inner.protocol_mode,
                &inner.protocol_version,
                params.clone(),
            ),
        });

        let line = serde_json::to_string(&request)
            .map_err(|e| VmError::Runtime(format!("MCP serialization error: {e}")))?;
        let StdioMcpClientInner { stdin, reader, .. } = &mut *inner;
        let (drained, partial) = tokio::time::timeout(
            MCP_TIMEOUT,
            write_line_draining(stdin, reader, line.as_bytes()),
        )
        .await
        .map_err(|_| {
            VmError::Runtime(format!(
                "MCP: server did not accept '{method}' within {}s",
                MCP_TIMEOUT.as_secs()
            ))
        })??;
        let msg = read_stdio_response(inner, server_name, method, id, drained, partial).await?;
        if maybe_retry_unsupported_protocol(inner.protocol_mode, &mut inner.protocol_version, &msg)
        {
            continue;
        }
        return Ok(msg);
    }

    Err(VmError::Runtime(
        "MCP request failed after protocol-version retry".into(),
    ))
}

/// Read one `\n`-terminated line into `buf` (which may already hold a partial
/// line from a previous cancelled call), enforcing `max_bytes` on the total
/// line length. Returns the number of bytes appended; `0` means EOF. The
/// future is cancel-safe: consumed bytes are always in `buf`, so a caller may
/// drop it at an await point and resume later with the same `buf`.
pub(crate) async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<usize, VmError> {
    let start_len = buf.len();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|e| VmError::Runtime(format!("MCP read error: {e}")))?;
        if available.is_empty() {
            return Ok(buf.len() - start_len);
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if buf.len().saturating_add(take) > max_bytes {
            return Err(VmError::Runtime(format!(
                "MCP protocol error: server sent a line exceeding the {} MiB limit",
                max_bytes / (1024 * 1024)
            )));
        }
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(buf.len() - start_len);
        }
    }
}

/// Write one JSON-RPC line while concurrently draining lines the server
/// emits. Writing and then reading on the same task can deadlock once both
/// pipe buffers fill: the server blocks writing stdout (nobody is reading it)
/// while we block writing a large request to stdin (the server is not reading
/// it until it can flush stdout). Draining keeps the server's stdout moving so
/// our write always completes.
///
/// Returns the drained messages — dispatched by the caller once the write has
/// finished, preserving the old strictly-sequential semantics — plus any
/// trailing partial line to resume reading from.
pub(crate) async fn write_line_draining<W, R>(
    stdin: &mut W,
    reader: &mut R,
    line: &[u8],
) -> Result<(Vec<serde_json::Value>, Vec<u8>), VmError>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    let write = async {
        stdin.write_all(line).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    };
    tokio::pin!(write);

    let mut drained = Vec::new();
    let mut drained_bytes = 0usize;
    let mut line_buf: Vec<u8> = Vec::new();
    let mut reader_eof = false;
    loop {
        tokio::select! {
            result = &mut write => {
                result.map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
                return Ok((drained, line_buf));
            }
            result = read_line_capped(reader, &mut line_buf, MCP_STDIO_MAX_LINE_BYTES), if !reader_eof => {
                let bytes_read = result?;
                if bytes_read == 0 {
                    // Server closed stdout. Keep driving the write: if the
                    // server is gone it fails with a broken pipe, and the
                    // caller surfaces the closed stream when it reads the
                    // response.
                    reader_eof = true;
                    continue;
                }
                drained_bytes = drained_bytes.saturating_add(bytes_read);
                if drained_bytes > MCP_STDIO_MAX_LINE_BYTES {
                    return Err(VmError::Runtime(format!(
                        "MCP protocol error: server flooded more than {} MiB while a request write was in flight",
                        MCP_STDIO_MAX_LINE_BYTES / (1024 * 1024)
                    )));
                }
                let text = std::str::from_utf8(&line_buf)
                    .map_err(|e| VmError::Runtime(format!("MCP read error: {e}")))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        drained.push(msg);
                    }
                }
                line_buf.clear();
            }
        }
    }
}

pub(crate) async fn write_stdio_json(
    stdin: &mut ChildStdin,
    message: &serde_json::Value,
) -> Result<(), VmError> {
    let line = serde_json::to_string(message)
        .map_err(|e| VmError::Runtime(format!("MCP serialization error: {e}")))?;
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
    stdin
        .flush()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP flush error: {e}")))
}

pub(crate) async fn read_stdio_response(
    inner: &mut StdioMcpClientInner,
    server_name: &str,
    method: &str,
    id: u64,
    drained: Vec<serde_json::Value>,
    mut line_buf: Vec<u8>,
) -> Result<serde_json::Value, VmError> {
    // One cumulative deadline for the whole response wait. Reading each line
    // under its own `MCP_TIMEOUT` let a server that interleaves notifications
    // reset the clock indefinitely, blocking an agent turn far beyond the
    // budget (harn#4390). The deadline is fixed once here so notifications no
    // longer extend it; a single synchronous call is bounded end to end.
    let deadline = tokio::time::Instant::now() + inner.response_deadline;
    let budget_secs = inner.response_deadline.as_secs();
    let timed_out = || {
        VmError::Runtime(format!(
            "MCP: server did not respond to '{method}' within {budget_secs}s"
        ))
    };

    // Messages drained while the request write was in flight come first, in
    // arrival order.
    for msg in drained {
        if let Some(response) = dispatch_stdio_message(inner, server_name, id, msg).await? {
            return Ok(response);
        }
    }
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(timed_out());
        }
        // `line_buf` may still hold a partial line left over from the write
        // drain; the first read resumes it.
        let bytes_read = tokio::time::timeout(
            remaining,
            read_line_capped(&mut inner.reader, &mut line_buf, MCP_STDIO_MAX_LINE_BYTES),
        )
        .await
        .map_err(|_| timed_out())??;

        if bytes_read == 0 {
            return Err(VmError::Runtime("MCP: server closed connection".into()));
        }

        let msg = {
            let text = std::str::from_utf8(&line_buf)
                .map_err(|e| VmError::Runtime(format!("MCP read error: {e}")))?;
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                serde_json::from_str::<serde_json::Value>(trimmed).ok()
            }
        };
        line_buf.clear();
        let Some(msg) = msg else {
            continue;
        };
        if let Some(response) = dispatch_stdio_message(inner, server_name, id, msg).await? {
            return Ok(response);
        }
    }
}

/// Route one inbound stdio message while waiting for the response to request
/// `id`: returns `Ok(Some(response))` when the message is that response,
/// otherwise handles it (notification relay, server-to-client request) and
/// returns `Ok(None)`.
async fn dispatch_stdio_message(
    inner: &mut StdioMcpClientInner,
    server_name: &str,
    id: u64,
    msg: serde_json::Value,
) -> Result<Option<serde_json::Value>, VmError> {
    if msg.get("id").is_none() {
        let _ = handle_inbound_client_request(server_name, &msg).await;
        return Ok(None);
    }

    if msg["id"].as_u64() == Some(id) && (msg.get("result").is_some() || msg.get("error").is_some())
    {
        return Ok(Some(msg));
    }

    if let Some(response) = handle_inbound_client_request(server_name, &msg).await {
        // Bound the write: responses to server-to-client requests are small,
        // but a wedged pipe must surface as an error, not an eternal hang.
        tokio::time::timeout(MCP_TIMEOUT, write_stdio_json(&mut inner.stdin, &response))
            .await
            .map_err(|_| {
                VmError::Runtime(format!(
                    "MCP: server did not accept a client response within {}s",
                    MCP_TIMEOUT.as_secs()
                ))
            })??;
    }
    Ok(None)
}

/// Handle a server-to-client request that arrived on the stream while
/// we were waiting for a response. Returns `Some(response)` to send
/// back to the server, or `None` if the message wasn't actually a
/// request (e.g. an unknown notification we should ignore).
pub(crate) async fn handle_inbound_client_request(
    server_name: &str,
    msg: &serde_json::Value,
) -> Option<serde_json::Value> {
    let method = msg.get("method").and_then(|value| value.as_str())?;
    if method == "notifications/progress" {
        relay_progress_notification(server_name, msg);
        return None;
    }
    if method == "notifications/message" {
        relay_log_notification(server_name, msg);
        return None;
    }
    if method == "notifications/resources/updated"
        || method == "notifications/resources/list_changed"
        || method == "notifications/tools/list_changed"
        || method == "notifications/prompts/list_changed"
    {
        relay_resource_notification(server_name, method, msg);
        return None;
    }
    if method == crate::mcp_elicit::ELICITATION_METHOD {
        return Some(crate::mcp_elicit::dispatch_inbound_elicitation(server_name, msg).await);
    }
    if method == crate::mcp_sampling::SAMPLING_METHOD {
        return Some(crate::mcp_sampling::dispatch_inbound_sampling(server_name, msg).await);
    }
    if method == crate::mcp_protocol::METHOD_ROOTS_LIST {
        let id = msg.get("id")?.clone();
        return Some(harn_roots_list_response(id));
    }
    client_request_rejection(msg)
}

pub(crate) async fn stdio_notify(
    inner: &mut StdioMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<(), VmError> {
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": request_params_for_protocol(
            inner.protocol_mode,
            &inner.protocol_version,
            params,
        ),
    });

    // Notifications are small (they fit the pipe buffer without needing the
    // server to drain it), so a plain bounded write suffices here — no
    // concurrent drain like `stdio_call_raw` requests get.
    tokio::time::timeout(
        MCP_TIMEOUT,
        write_stdio_json(&mut inner.stdin, &notification),
    )
    .await
    .map_err(|_| {
        VmError::Runtime(format!(
            "MCP: server did not accept '{method}' within {}s",
            MCP_TIMEOUT.as_secs()
        ))
    })??;
    Ok(())
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
    let mut legacy_session_retry_used = false;
    let mut auth_retry_used = false;
    let mut protocol_version_retry_used = false;
    loop {
        let auth_completion_rx = crate::mcp_oauth::subscribe_authorization_completions();
        let response = send_http_request_once(inner, method, params.clone(), id).await?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if let Some(protocol_version) = headers
            .get(RC_HEADER_PROTOCOL_VERSION)
            .and_then(|v| v.to_str().ok())
        {
            inner.protocol_version = protocol_version.to_string();
        }
        if inner.protocol_mode == McpProtocolMode::Legacy {
            if let Some(session_id) = headers.get("MCP-Session-Id").and_then(|v| v.to_str().ok()) {
                inner.session_id = Some(session_id.to_string());
            }
        }

        if inner.protocol_mode == McpProtocolMode::Legacy
            && status == 404
            && inner.session_id.is_some()
            && method != "initialize"
            && !legacy_session_retry_used
        {
            legacy_session_retry_used = true;
            inner.session_id = None;
            inner.abort_get_stream();
            reinitialize_http_client(inner).await?;
            continue;
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
            if should_fallback_to_legacy_http_discovery(inner.protocol_mode, method, status) {
                return Ok(http_discovery_fallback_response(id));
            }
            if status >= 400 {
                return Err(VmError::Runtime(format!(
                    "MCP HTTP request returned {status} with an empty response body"
                )));
            }
            if status < 400 {
                ensure_http_get_stream(inner, server_name);
            }
            return Ok(serde_json::Value::Null);
        }

        let msg = match parse_http_response_body(inner, server_name, &body, status, id).await {
            Ok(msg) => msg,
            Err(_)
                if should_fallback_to_legacy_http_discovery(
                    inner.protocol_mode,
                    method,
                    status,
                ) =>
            {
                return Ok(http_discovery_fallback_response(id));
            }
            Err(err) => return Err(err),
        };

        if maybe_retry_unsupported_protocol(inner.protocol_mode, &mut inner.protocol_version, &msg)
            && !protocol_version_retry_used
        {
            protocol_version_retry_used = true;
            continue;
        }

        ensure_http_get_stream(inner, server_name);
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
    inner.abort_get_stream();
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
    let request_params =
        request_params_for_protocol(inner.protocol_mode, &inner.protocol_version, params);
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
        inner.protocol_mode,
        &inner.protocol_version,
        legacy_session_id(inner),
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

pub(crate) fn ensure_http_get_stream(inner: &mut HttpMcpClientInner, server_name: &str) {
    if inner.protocol_mode == McpProtocolMode::Modern {
        return;
    }
    if server_name.is_empty() {
        return;
    }
    if inner
        .get_stream_task
        .as_ref()
        .is_some_and(|task| !task.is_finished())
    {
        return;
    }

    let config = HttpStreamConfig {
        client: inner.client.clone(),
        url: inner.url.clone(),
        auth_token: inner.auth_token.clone(),
        protocol_mode: inner.protocol_mode,
        protocol_version: inner.protocol_version.clone(),
        session_id: inner.session_id.clone(),
        proxy_server_name: inner.proxy_server_name.clone(),
        server_name: server_name.to_string(),
    };
    inner.get_stream_task = Some(tokio::task::spawn_local(run_http_get_stream(config)));
}

pub(crate) async fn run_http_get_stream(config: HttpStreamConfig) {
    let request = apply_http_headers(
        config
            .client
            .get(&config.url)
            .header("Accept", "text/event-stream"),
        &config.auth_token,
        config.protocol_mode,
        &config.protocol_version,
        config.session_id.as_deref(),
        None,
        None,
        &BTreeMap::new(),
    );
    let Ok(mut stream) = EventSource::new(request) else {
        return;
    };

    while let Some(event) = stream.next().await {
        match event {
            Ok(SseEvent::Open) => {}
            Ok(SseEvent::Message(message)) => {
                if message.data.trim().is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(&message.data) else {
                    tracing::debug!("MCP HTTP GET stream received non-JSON event");
                    continue;
                };
                if let Some(response) =
                    handle_inbound_client_request(&config.server_name, &msg).await
                {
                    let _ = post_http_jsonrpc_payload(&config, response).await;
                }
            }
            Err(error) => {
                tracing::debug!(
                    "MCP HTTP GET stream ended with error: {}",
                    crate::egress::redact_diagnostic_text(&error.to_string())
                );
                break;
            }
        }
    }
    stream.close();
}

pub(crate) async fn post_http_jsonrpc_payload(
    config: &HttpStreamConfig,
    payload: serde_json::Value,
) -> Result<(), VmError> {
    let payload = wrap_http_payload(payload, config.proxy_server_name.as_deref());
    let request = config
        .client
        .post(&config.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&payload)
        .timeout(MCP_TIMEOUT);
    let request = apply_http_headers(
        request,
        &config.auth_token,
        config.protocol_mode,
        &config.protocol_version,
        config.session_id.as_deref(),
        None,
        None,
        &BTreeMap::new(),
    );
    let response = request.send().await.map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP response POST error: {}",
            crate::egress::redact_reqwest_error(&e)
        ))
    })?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(VmError::Runtime(format!(
            "MCP HTTP response POST returned {}",
            response.status()
        )))
    }
}

pub(crate) fn apply_http_headers(
    mut request: reqwest::RequestBuilder,
    auth_token: &Option<String>,
    protocol_mode: McpProtocolMode,
    protocol_version: &str,
    session_id: Option<&str>,
    method: Option<&str>,
    params: Option<&serde_json::Value>,
    tool_headers: &BTreeMap<String, Vec<McpToolHeader>>,
) -> reqwest::RequestBuilder {
    request = request.header(RC_HEADER_PROTOCOL_VERSION, protocol_version);
    if let Some(token) = auth_token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if protocol_mode == McpProtocolMode::Legacy {
        if let Some(session_id) = session_id {
            request = request.header(MCP_SESSION_HEADER_LEGACY, session_id);
        }
    }
    if protocol_mode == McpProtocolMode::Modern {
        if let Some(method) = method {
            request = request.header(RC_HEADER_METHOD, method);
            if let Some(params) = params {
                if let Some(name) = rc_name_header_value(method, params) {
                    request = request.header(RC_HEADER_NAME, name);
                }
            }
            if method == "tools/call" {
                request = apply_mcp_tool_parameter_headers(request, params, tool_headers);
            }
        }
    }
    request
}

pub(crate) fn legacy_session_id(inner: &HttpMcpClientInner) -> Option<&str> {
    (inner.protocol_mode == McpProtocolMode::Legacy)
        .then_some(inner.session_id.as_deref())
        .flatten()
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

pub(crate) async fn reinitialize_http_client(
    inner: &mut HttpMcpClientInner,
) -> Result<(), VmError> {
    let initialize = send_http_request_once(
        inner,
        "initialize",
        legacy_initialize_params(&inner.protocol_version),
        Some(0),
    )
    .await?;
    if let Some(protocol_version) = initialize
        .headers()
        .get(RC_HEADER_PROTOCOL_VERSION)
        .and_then(|v| v.to_str().ok())
    {
        inner.protocol_version = protocol_version.to_string();
    }
    if inner.protocol_mode == McpProtocolMode::Legacy {
        if let Some(session_id) = initialize
            .headers()
            .get(MCP_SESSION_HEADER_LEGACY)
            .and_then(|v| v.to_str().ok())
        {
            inner.session_id = Some(session_id.to_string());
        }
    }
    let status = initialize.status().as_u16();
    let body = initialize.text().await.map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP read error: {}",
            crate::egress::redact_reqwest_error(&e)
        ))
    })?;
    let msg = parse_http_response_body(inner, "", &body, status, Some(0)).await?;
    if status >= 400 {
        return Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)));
    }
    let _ = parse_jsonrpc_result(msg)?;
    let response = send_http_request_once(
        inner,
        "notifications/initialized",
        serde_json::json!({}),
        None,
    )
    .await?;
    let status = response.status().as_u16();
    if let Some(protocol_version) = response
        .headers()
        .get(RC_HEADER_PROTOCOL_VERSION)
        .and_then(|v| v.to_str().ok())
    {
        inner.protocol_version = protocol_version.to_string();
    }
    if inner.protocol_mode == McpProtocolMode::Legacy {
        if let Some(session_id) = response
            .headers()
            .get(MCP_SESSION_HEADER_LEGACY)
            .and_then(|v| v.to_str().ok())
        {
            inner.session_id = Some(session_id.to_string());
        }
    }
    let body = response.text().await.map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP read error: {}",
            crate::egress::redact_reqwest_error(&e)
        ))
    })?;
    if body.trim().is_empty() || status < 400 {
        return Ok(());
    }
    let msg = parse_http_response_body(inner, "", &body, status, None).await?;
    Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)))
}

pub(crate) async fn parse_http_response_body(
    inner: &HttpMcpClientInner,
    server_name: &str,
    body: &str,
    status: u16,
    request_id: Option<u64>,
) -> Result<serde_json::Value, VmError> {
    if body.trim_start().starts_with("event:") || body.trim_start().starts_with("data:") {
        return parse_sse_jsonrpc_body(inner, server_name, body, request_id).await;
    }
    serde_json::from_str(body).map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP response parse error (status {status}): {e}"
        ))
    })
}

pub(crate) async fn parse_sse_jsonrpc_body(
    inner: &HttpMcpClientInner,
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

    let config = HttpStreamConfig {
        client: inner.client.clone(),
        url: inner.url.clone(),
        auth_token: inner.auth_token.clone(),
        protocol_mode: inner.protocol_mode,
        protocol_version: inner.protocol_version.clone(),
        session_id: inner.session_id.clone(),
        proxy_server_name: inner.proxy_server_name.clone(),
        server_name: server_name.to_string(),
    };

    let mut fallback = None;
    for message in messages {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message) {
            if request_id.is_some()
                && value["id"].as_u64() == request_id
                && (value.get("result").is_some() || value.get("error").is_some())
            {
                return Ok(value);
            }
            if let Some(response) = handle_inbound_client_request(server_name, &value).await {
                let _ = post_http_jsonrpc_payload(&config, response).await;
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
