//! MCP (Model Context Protocol) client for connecting to external tool servers.
//!
//! Supports stdio transport and streamable HTTP-style request/response transport.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine;
use futures::StreamExt;
use reqwest_eventsource::{Event as SseEvent, EventSource};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use crate::mcp_protocol::{
    cache_hints_to_json, rc_name_header_value, McpCacheHint, McpProtocolMode,
    DRAFT_PROTOCOL_VERSION, MCP_SESSION_HEADER_LEGACY, PROTOCOL_VERSION, RC_HEADER_METHOD,
    RC_HEADER_NAME, RC_HEADER_PROTOCOL_VERSION, RC_META_KEY_CLIENT_CAPABILITIES,
    RC_META_KEY_CLIENT_INFO, RC_META_KEY_PROTOCOL_VERSION, RESULT_TYPE_INPUT_REQUIRED,
    UNSUPPORTED_PROTOCOL_VERSION_CODE,
};

const X_MCP_HEADER: &str = "x-mcp-header";
const MCP_INPUT_REQUIRED_MAX_ROUNDS: usize = 8;

/// Default timeout for MCP requests (60 seconds).
const MCP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum McpTransport {
    Stdio,
    Http,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    #[serde(default = "default_transport")]
    transport: McpTransport,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub protocol_mode: Option<String>,
    #[serde(default)]
    pub proxy_server_name: Option<String>,
}

fn default_transport() -> McpTransport {
    McpTransport::Stdio
}

/// Internal state for an MCP client connection.
enum McpClientInner {
    Stdio(StdioMcpClientInner),
    Http(HttpMcpClientInner),
}

struct StdioMcpClientInner {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
}

struct HttpMcpClientInner {
    client: reqwest::Client,
    url: String,
    auth_token: Option<String>,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
    session_id: Option<String>,
    next_id: u64,
    proxy_server_name: Option<String>,
    get_stream_task: Option<tokio::task::JoinHandle<()>>,
    tool_headers: BTreeMap<String, Vec<McpToolHeader>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct McpToolHeader {
    parameter: String,
    header_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpRoot {
    path: String,
    uri: String,
    name: String,
}

impl McpRoot {
    fn protocol_json(&self) -> serde_json::Value {
        serde_json::json!({
            "uri": self.uri,
            "name": self.name,
        })
    }

    fn script_json(&self) -> serde_json::Value {
        serde_json::json!({
            "uri": self.uri,
            "name": self.name,
            "path": self.path,
        })
    }
}

impl HttpMcpClientInner {
    fn abort_get_stream(&mut self) {
        if let Some(task) = self.get_stream_task.take() {
            task.abort();
        }
    }
}

impl Drop for StdioMcpClientInner {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Drop for HttpMcpClientInner {
    fn drop(&mut self) {
        self.abort_get_stream();
    }
}

/// Handle to an MCP client connection, stored in VmValue.
#[derive(Clone)]
pub struct VmMcpClientHandle {
    pub name: String,
    inner: Arc<Mutex<Option<McpClientInner>>>,
    last_roots: Arc<Mutex<Vec<McpRoot>>>,
    pub(crate) initialize_result: Arc<Mutex<Option<serde_json::Value>>>,
    cache_hints: Arc<Mutex<BTreeMap<String, McpCacheHint>>>,
}

impl std::fmt::Debug for VmMcpClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "McpClient({})", self.name)
    }
}

impl VmMcpClientHandle {
    async fn protocol_mode(&self) -> Result<McpProtocolMode, VmError> {
        let guard = self.inner.lock().await;
        let inner = guard
            .as_ref()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;
        Ok(match inner {
            McpClientInner::Stdio(inner) => inner.protocol_mode,
            McpClientInner::Http(inner) => inner.protocol_mode,
        })
    }

    async fn protocol_version(&self) -> Result<String, VmError> {
        let guard = self.inner.lock().await;
        let inner = guard
            .as_ref()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;
        Ok(match inner {
            McpClientInner::Stdio(inner) => inner.protocol_version.clone(),
            McpClientInner::Http(inner) => inner.protocol_version.clone(),
        })
    }

    async fn switch_to_legacy_protocol(&self) -> Result<(), VmError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;
        match inner {
            McpClientInner::Stdio(inner) => {
                inner.protocol_mode = McpProtocolMode::Legacy;
                inner.protocol_version = PROTOCOL_VERSION.to_string();
            }
            McpClientInner::Http(inner) => {
                inner.protocol_mode = McpProtocolMode::Legacy;
                inner.protocol_version = PROTOCOL_VERSION.to_string();
            }
        }
        Ok(())
    }

    pub(crate) async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        let msg = self.call_raw(method, params).await?;
        parse_jsonrpc_result(msg)
    }

    async fn call_raw(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        if method != "initialize" && method != "server/discover" {
            self.notify_roots_list_changed_if_needed().await?;
        }
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        match inner {
            McpClientInner::Stdio(inner) => stdio_call_raw(inner, &self.name, method, params).await,
            McpClientInner::Http(inner) => http_call_raw(inner, &self.name, method, params).await,
        }
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), VmError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        match inner {
            McpClientInner::Stdio(inner) => stdio_notify(inner, method, params).await,
            McpClientInner::Http(inner) => http_notify(inner, &self.name, method, params).await,
        }
    }

    pub(crate) async fn disconnect(&self) -> Result<(), VmError> {
        let mut guard = self.inner.lock().await;
        if let Some(inner) = guard.take() {
            match inner {
                McpClientInner::Stdio(mut inner) => {
                    let _ = inner.child.kill().await;
                }
                McpClientInner::Http(mut inner) => {
                    inner.abort_get_stream();
                }
            }
        }
        Ok(())
    }

    async fn notify_roots_list_changed_if_needed(&self) -> Result<(), VmError> {
        if self.protocol_mode().await? == McpProtocolMode::Modern {
            return Ok(());
        }
        let roots = current_mcp_roots();
        let mut last_roots = self.last_roots.lock().await;
        if *last_roots == roots {
            return Ok(());
        }

        self.notify(
            crate::mcp_protocol::METHOD_ROOTS_LIST_CHANGED_NOTIFICATION,
            serde_json::json!({}),
        )
        .await?;
        *last_roots = roots;
        Ok(())
    }

    async fn record_cache_hint(&self, method: &str, result: &serde_json::Value) {
        let Some(hint) = McpCacheHint::from_result(result) else {
            return;
        };
        self.cache_hints
            .lock()
            .await
            .insert(method.to_string(), hint);
    }

    async fn store_http_tool_headers(&self, tools: &[serde_json::Value]) {
        let mut valid_headers = BTreeMap::new();
        let mut valid_tools = std::collections::BTreeSet::new();
        for tool in tools {
            let Some(name) = tool.get("name").and_then(|value| value.as_str()) else {
                continue;
            };
            match extract_tool_headers(tool) {
                Ok(headers) => {
                    valid_tools.insert(name.to_string());
                    if !headers.is_empty() {
                        valid_headers.insert(name.to_string(), headers);
                    }
                }
                Err(reason) => {
                    tracing::warn!(tool = name, %reason, "rejecting MCP tool with invalid x-mcp-header annotation");
                }
            }
        }

        let mut guard = self.inner.lock().await;
        if let Some(McpClientInner::Http(inner)) = guard.as_mut() {
            inner
                .tool_headers
                .retain(|tool, _| valid_tools.contains(tool));
            inner.tool_headers.extend(valid_headers);
        }
    }
}

async fn stdio_call_raw(
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

        write_stdio_json(&mut inner.stdin, &request).await?;
        let msg = read_stdio_response(inner, server_name, method, id).await?;
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

async fn write_stdio_json(
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

async fn read_stdio_response(
    inner: &mut StdioMcpClientInner,
    server_name: &str,
    method: &str,
    id: u64,
) -> Result<serde_json::Value, VmError> {
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        let bytes_read = tokio::time::timeout(MCP_TIMEOUT, inner.reader.read_line(&mut line_buf))
            .await
            .map_err(|_| {
                VmError::Runtime(format!(
                    "MCP: server did not respond to '{method}' within {}s",
                    MCP_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| VmError::Runtime(format!("MCP read error: {e}")))?;

        if bytes_read == 0 {
            return Err(VmError::Runtime("MCP: server closed connection".into()));
        }

        let trimmed = line_buf.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if msg.get("id").is_none() {
            let _ = handle_inbound_client_request(server_name, &msg).await;
            continue;
        }

        if msg["id"].as_u64() == Some(id)
            && (msg.get("result").is_some() || msg.get("error").is_some())
        {
            return Ok(msg);
        }

        let response = match handle_inbound_client_request(server_name, &msg).await {
            Some(response) => response,
            None => continue,
        };
        write_stdio_json(&mut inner.stdin, &response).await?;
    }
}

/// Handle a server-to-client request that arrived on the stream while
/// we were waiting for a response. Returns `Some(response)` to send
/// back to the server, or `None` if the message wasn't actually a
/// request (e.g. an unknown notification we should ignore).
async fn handle_inbound_client_request(
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

/// Forward an MCP `notifications/progress` event into the originating
/// agent session's inbox. Correlation by `progressToken`: every
/// outgoing `tools/call` issued by [`call_mcp_tool`] registers a fresh
/// token in [`client_progress`]; this function looks up the inbound
/// notification's token and routes it back to whichever session is
/// bound to it. Falls back to the thread-local current session for
/// notifications without a token (e.g. servers that emit progress
/// unsolicited). Drops silently when neither a token mapping nor a
/// current session is present.
fn relay_progress_notification(server_name: &str, msg: &serde_json::Value) {
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
    let content = serde_json::to_string(&payload).unwrap_or_default();
    crate::orchestration::agent_inbox::push(
        &session_id,
        "mcp_progress",
        &content,
        "mcp.notifications/progress",
    );
}

fn relay_log_notification(server_name: &str, msg: &serde_json::Value) {
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
    let content = serde_json::to_string(&payload).unwrap_or_default();
    crate::orchestration::agent_inbox::push(
        &session_id,
        "mcp_log",
        &content,
        "mcp.notifications/message",
    );
}

fn relay_resource_notification(server_name: &str, method: &str, msg: &serde_json::Value) {
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    let payload = serde_json::json!({
        "server": server_name,
        "method": method,
        "params": msg.get("params").cloned().unwrap_or(serde_json::Value::Null),
    });
    let content = serde_json::to_string(&payload).unwrap_or_default();
    crate::orchestration::agent_inbox::push(
        &session_id,
        "mcp_resource_change",
        &content,
        "mcp.notifications",
    );
}

async fn stdio_notify(
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

    let line = serde_json::to_string(&notification)
        .map_err(|e| VmError::Runtime(format!("MCP serialization error: {e}")))?;
    inner
        .stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
    inner
        .stdin
        .write_all(b"\n")
        .await
        .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
    inner
        .stdin
        .flush()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP flush error: {e}")))?;
    Ok(())
}

async fn http_call_raw(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let id = inner.next_id;
    inner.next_id += 1;
    send_http_request(inner, server_name, method, params, Some(id)).await
}

async fn http_notify(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<(), VmError> {
    let _ = send_http_request(inner, server_name, method, params, None).await?;
    Ok(())
}

async fn send_http_request(
    inner: &mut HttpMcpClientInner,
    server_name: &str,
    method: &str,
    params: serde_json::Value,
    id: Option<u64>,
) -> Result<serde_json::Value, VmError> {
    for attempt in 0..2 {
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
            && attempt == 0
        {
            inner.session_id = None;
            inner.abort_get_stream();
            reinitialize_http_client(inner).await?;
            continue;
        }

        if status == 401 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "MCP authorization required",
            ))));
        }

        let body = response
            .text()
            .await
            .map_err(|e| VmError::Runtime(format!("MCP HTTP read error: {e}")))?;

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
            && attempt == 0
        {
            continue;
        }

        ensure_http_get_stream(inner, server_name);
        if status >= 400 && id.is_none() {
            return Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)));
        }
        return Ok(msg);
    }

    Err(VmError::Runtime("MCP HTTP request failed".into()))
}

async fn send_http_request_once(
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

    let request = inner
        .client
        .post(&inner.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&payload);
    let request = apply_http_headers(
        request,
        &inner.auth_token,
        inner.protocol_mode,
        &inner.protocol_version,
        legacy_session_id(inner),
        Some(method),
        payload.get("params"),
        &inner.tool_headers,
    );

    request
        .timeout(MCP_TIMEOUT)
        .send()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP request error: {e}")))
}

fn ensure_http_get_stream(inner: &mut HttpMcpClientInner, server_name: &str) {
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

#[derive(Clone)]
struct HttpStreamConfig {
    client: reqwest::Client,
    url: String,
    auth_token: Option<String>,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
    session_id: Option<String>,
    proxy_server_name: Option<String>,
    server_name: String,
}

async fn run_http_get_stream(config: HttpStreamConfig) {
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
                tracing::debug!("MCP HTTP GET stream ended with error: {error}");
                break;
            }
        }
    }
    stream.close();
}

async fn post_http_jsonrpc_payload(
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
    let response = request
        .send()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP response POST error: {e}")))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(VmError::Runtime(format!(
            "MCP HTTP response POST returned {}",
            response.status()
        )))
    }
}

fn apply_http_headers(
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

fn legacy_session_id(inner: &HttpMcpClientInner) -> Option<&str> {
    (inner.protocol_mode == McpProtocolMode::Legacy)
        .then_some(inner.session_id.as_deref())
        .flatten()
}

fn apply_mcp_tool_parameter_headers(
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

fn wrap_http_payload(
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

async fn reinitialize_http_client(inner: &mut HttpMcpClientInner) -> Result<(), VmError> {
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
    let body = initialize
        .text()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP read error: {e}")))?;
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
    let body = response
        .text()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP read error: {e}")))?;
    if body.trim().is_empty() || status < 400 {
        return Ok(());
    }
    let msg = parse_http_response_body(inner, "", &body, status, None).await?;
    Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)))
}

async fn parse_http_response_body(
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

async fn parse_sse_jsonrpc_body(
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

fn parse_jsonrpc_result(msg: serde_json::Value) -> Result<serde_json::Value, VmError> {
    if let Some(error) = msg.get("error") {
        return Err(jsonrpc_error_to_vm_error(error));
    }
    Ok(msg
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

fn jsonrpc_error_to_vm_error(error: &serde_json::Value) -> VmError {
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown MCP error");
    let code = error.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    VmError::Thrown(VmValue::String(Rc::from(format!(
        "MCP error ({code}): {message}"
    ))))
}

fn client_request_rejection(msg: &serde_json::Value) -> Option<serde_json::Value> {
    let request_id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(|value| value.as_str())?;
    Some(crate::jsonrpc::error_response(
        request_id,
        -32601,
        &format!("Method not found: {method}"),
    ))
}

fn harn_roots_list_response(id: serde_json::Value) -> serde_json::Value {
    crate::jsonrpc::response(
        id,
        serde_json::json!({
            "roots": current_mcp_roots()
                .iter()
                .map(McpRoot::protocol_json)
                .collect::<Vec<_>>()
        }),
    )
}

pub(crate) fn current_mcp_roots() -> Vec<McpRoot> {
    compact_root_paths(current_mcp_root_candidates())
        .into_iter()
        .filter_map(|path| {
            let uri = url::Url::from_file_path(&path).ok()?.to_string();
            Some(McpRoot {
                name: root_display_name(&path),
                path: path.to_string_lossy().into_owned(),
                uri,
            })
        })
        .collect()
}

fn current_mcp_root_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(context) = crate::stdlib::process::current_execution_context() {
        if let Some(path) = non_empty_path(context.worktree_path.as_deref()) {
            candidates.push(path);
        }
        if let Some(cwd) = non_empty_path(context.cwd.as_deref()) {
            push_project_root_or_path(&mut candidates, cwd);
        }
        if let Some(source_dir) = non_empty_path(context.source_dir.as_deref()) {
            push_project_root_or_path(&mut candidates, source_dir);
        }
    } else {
        push_project_root_or_path(
            &mut candidates,
            crate::stdlib::process::execution_root_path(),
        );
        push_project_root_or_path(&mut candidates, crate::stdlib::process::source_root_path());
    }

    if candidates.is_empty() {
        candidates.push(crate::stdlib::process::execution_root_path());
    }
    candidates
}

fn non_empty_path(raw: Option<&str>) -> Option<PathBuf> {
    raw.filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

fn push_project_root_or_path(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    let normalized = crate::stdlib::process::normalize_context_path(&path);
    match crate::stdlib::process::find_project_root(&normalized) {
        Some(root) => candidates.push(root),
        None => candidates.push(normalized),
    }
}

fn compact_root_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut normalized = paths
        .into_iter()
        .map(normalize_root_path)
        .collect::<Vec<_>>();
    normalized.sort_by_key(|path| {
        (
            path.components().count(),
            path.to_string_lossy().to_string(),
        )
    });

    let mut roots: Vec<PathBuf> = Vec::new();
    for path in normalized {
        if roots
            .iter()
            .any(|existing| path == *existing || path.starts_with(existing))
        {
            continue;
        }
        roots.push(path);
    }
    roots
}

fn normalize_root_path(path: PathBuf) -> PathBuf {
    let absolute = crate::stdlib::process::normalize_context_path(&path);
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

fn root_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

async fn mcp_connect_stdio_impl(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
) -> Result<VmMcpClientHandle, VmError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .envs(env);
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "mcp_connect: failed to spawn '{command}': {e}"
        ))))
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| VmError::Runtime("mcp_connect: failed to open stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VmError::Runtime("mcp_connect: failed to open stdout".into()))?;

    let handle = VmMcpClientHandle {
        name: command.to_string(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Stdio(
            StdioMcpClientInner {
                child,
                stdin,
                reader: BufReader::new(stdout),
                next_id: 1,
                protocol_mode,
                protocol_version,
            },
        )))),
        last_roots: Arc::new(Mutex::new(Vec::new())),
        initialize_result: Arc::new(Mutex::new(None)),
        cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
    };

    initialize_client(&handle).await?;
    Ok(handle)
}

async fn mcp_connect_http_impl(spec: &McpServerSpec) -> Result<VmMcpClientHandle, VmError> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| VmError::Runtime(format!("MCP HTTP client error: {e}")))?;
    let protocol_mode = resolve_protocol_mode(
        spec.protocol_mode.as_deref(),
        spec.protocol_version.as_deref(),
    )?;
    let protocol_version = spec
        .protocol_version
        .clone()
        .unwrap_or_else(|| default_protocol_version(protocol_mode).to_string());

    let handle = VmMcpClientHandle {
        name: spec.name.clone(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Http(HttpMcpClientInner {
            client,
            url: spec.url.clone(),
            auth_token: spec.auth_token.clone(),
            protocol_mode,
            protocol_version,
            session_id: None,
            next_id: 1,
            proxy_server_name: spec.proxy_server_name.clone(),
            get_stream_task: None,
            tool_headers: BTreeMap::new(),
        })))),
        last_roots: Arc::new(Mutex::new(Vec::new())),
        initialize_result: Arc::new(Mutex::new(None)),
        cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
    };

    initialize_client(&handle).await?;
    Ok(handle)
}

async fn initialize_client(handle: &VmMcpClientHandle) -> Result<(), VmError> {
    if handle.protocol_mode().await? == McpProtocolMode::Modern {
        let discover = handle
            .call_raw("server/discover", serde_json::json!({}))
            .await?;
        if is_method_not_found_response(&discover) {
            handle.switch_to_legacy_protocol().await?;
            return initialize_legacy_client(handle).await;
        }
        let discover_result = parse_jsonrpc_result(discover)?;
        *handle.initialize_result.lock().await = Some(discover_result);
        return Ok(());
    }

    initialize_legacy_client(handle).await
}

async fn initialize_legacy_client(handle: &VmMcpClientHandle) -> Result<(), VmError> {
    let protocol_version = handle.protocol_version().await?;
    let initialize_result = handle
        .call("initialize", legacy_initialize_params(&protocol_version))
        .await?;
    *handle.initialize_result.lock().await = Some(initialize_result);

    handle
        .notify("notifications/initialized", serde_json::json!({}))
        .await?;

    Ok(())
}

pub(crate) fn vm_value_to_serde(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(n) => serde_json::json!(*n),
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_serde).collect())
        }
        VmValue::Dict(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

fn resolve_protocol_mode(
    protocol_mode: Option<&str>,
    protocol_version: Option<&str>,
) -> Result<McpProtocolMode, VmError> {
    let normalized = protocol_mode.map(|value| value.trim().to_ascii_lowercase());
    match normalized.as_deref() {
        Some("legacy") | Some("2025") | Some("2025-11-25") => Ok(McpProtocolMode::Legacy),
        Some("rc") | Some("modern") | Some("draft") | Some("draft-2026-v1") => {
            Ok(McpProtocolMode::Modern)
        }
        Some(other) => Err(VmError::Runtime(format!(
            "mcp_connect: unsupported protocol_mode {other:?}; expected \"legacy\" or \"rc\""
        ))),
        None if protocol_version == Some(DRAFT_PROTOCOL_VERSION) => Ok(McpProtocolMode::Modern),
        None => Ok(McpProtocolMode::Legacy),
    }
}

struct McpConnectOptions {
    protocol_mode: McpProtocolMode,
    protocol_version: String,
}

fn mcp_connect_options(value: Option<&VmValue>) -> Result<McpConnectOptions, VmError> {
    let Some(value) = value else {
        return Ok(McpConnectOptions {
            protocol_mode: McpProtocolMode::Legacy,
            protocol_version: PROTOCOL_VERSION.to_string(),
        });
    };
    let VmValue::Dict(options) = value else {
        return Err(VmError::Runtime(format!(
            "mcp_connect: options must be a dict, got {}",
            value.type_name()
        )));
    };
    let protocol_mode_value = options.get("protocol_mode").map(|value| value.display());
    let protocol_version_value = options.get("protocol_version").map(|value| value.display());
    let protocol_mode = resolve_protocol_mode(
        protocol_mode_value.as_deref(),
        protocol_version_value.as_deref(),
    )?;
    let protocol_version = protocol_version_value
        .unwrap_or_else(|| default_protocol_version(protocol_mode).to_string());
    Ok(McpConnectOptions {
        protocol_mode,
        protocol_version,
    })
}

fn default_protocol_version(mode: McpProtocolMode) -> &'static str {
    match mode {
        McpProtocolMode::Legacy => PROTOCOL_VERSION,
        McpProtocolMode::Modern => DRAFT_PROTOCOL_VERSION,
    }
}

fn legacy_initialize_params(protocol_version: &str) -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": legacy_client_capabilities(),
        "clientInfo": client_info(),
    })
}

fn client_info() -> serde_json::Value {
    serde_json::json!({
        "name": "harn",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn legacy_client_capabilities() -> serde_json::Value {
    serde_json::json!({
        "elicitation": {},
        "roots": {
            "listChanged": true,
        },
        "sampling": {},
    })
}

fn modern_client_capabilities() -> serde_json::Value {
    serde_json::json!({
        "elicitation": {},
        "roots": {},
        "sampling": {},
    })
}

fn request_params_for_protocol(
    protocol_mode: McpProtocolMode,
    protocol_version: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    if protocol_mode == McpProtocolMode::Legacy {
        return params;
    }

    let mut object = match params {
        serde_json::Value::Object(object) => object,
        serde_json::Value::Null => serde_json::Map::new(),
        other => serde_json::Map::from_iter([("value".to_string(), other)]),
    };
    let mut meta = object
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        RC_META_KEY_PROTOCOL_VERSION.to_string(),
        serde_json::Value::String(protocol_version.to_string()),
    );
    meta.insert(RC_META_KEY_CLIENT_INFO.to_string(), client_info());
    meta.insert(
        RC_META_KEY_CLIENT_CAPABILITIES.to_string(),
        modern_client_capabilities(),
    );
    object.insert("_meta".to_string(), serde_json::Value::Object(meta));
    serde_json::Value::Object(object)
}

fn maybe_retry_unsupported_protocol(
    protocol_mode: McpProtocolMode,
    protocol_version: &mut String,
    msg: &serde_json::Value,
) -> bool {
    if protocol_mode != McpProtocolMode::Modern {
        return false;
    }
    let Some(error) = msg.get("error") else {
        return false;
    };
    if error.get("code").and_then(|value| value.as_i64()) != Some(UNSUPPORTED_PROTOCOL_VERSION_CODE)
    {
        return false;
    }
    let supported = error
        .get("data")
        .and_then(|data| data.get("supported"))
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();
    let Some(selected) = select_supported_protocol_version(&supported) else {
        return false;
    };
    if selected == protocol_version {
        return false;
    }
    *protocol_version = selected.to_string();
    true
}

fn should_fallback_to_legacy_http_discovery(
    protocol_mode: McpProtocolMode,
    method: &str,
    status: u16,
) -> bool {
    protocol_mode == McpProtocolMode::Modern
        && method == "server/discover"
        && matches!(status, 400 | 404 | 405)
}

fn http_discovery_fallback_response(id: Option<u64>) -> serde_json::Value {
    crate::jsonrpc::error_response(
        id.map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
        -32601,
        "Modern MCP discovery was not recognized",
    )
}

fn select_supported_protocol_version(supported: &[&str]) -> Option<&'static str> {
    [DRAFT_PROTOCOL_VERSION, PROTOCOL_VERSION]
        .into_iter()
        .find(|candidate| supported.iter().any(|value| value == candidate))
}

fn is_method_not_found_response(msg: &serde_json::Value) -> bool {
    msg.get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_i64())
        == Some(-32601)
}

fn extract_tool_headers(tool: &serde_json::Value) -> Result<Vec<McpToolHeader>, String> {
    let Some(properties) = tool
        .get("inputSchema")
        .and_then(|schema| schema.get("properties"))
        .and_then(|value| value.as_object())
    else {
        return Ok(Vec::new());
    };

    let mut headers = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (parameter, schema) in properties {
        let Some(header_name) = schema.get(X_MCP_HEADER).and_then(|value| value.as_str()) else {
            continue;
        };
        validate_mcp_header_annotation(parameter, header_name, schema, &mut seen)?;
        headers.push(McpToolHeader {
            parameter: parameter.clone(),
            header_name: format!("Mcp-Param-{header_name}"),
        });
    }
    Ok(headers)
}

fn filter_tools_for_client(tools: &[serde_json::Value]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("<unnamed>");
            match extract_tool_headers(tool) {
                Ok(_) => Some(tool.clone()),
                Err(reason) => {
                    tracing::warn!(tool = name, %reason, "excluding MCP tool from tools/list");
                    None
                }
            }
        })
        .collect()
}

fn validate_mcp_header_annotation(
    parameter: &str,
    header_name: &str,
    schema: &serde_json::Value,
    seen: &mut std::collections::BTreeSet<String>,
) -> Result<(), String> {
    if header_name.is_empty() {
        return Err(format!("{parameter}: x-mcp-header must not be empty"));
    }
    if !header_name.is_ascii() || header_name.bytes().any(|byte| matches!(byte, b' ' | b':')) {
        return Err(format!(
            "{parameter}: x-mcp-header must be ASCII and exclude space or colon"
        ));
    }
    if reqwest::header::HeaderName::from_bytes(format!("Mcp-Param-{header_name}").as_bytes())
        .is_err()
    {
        return Err(format!(
            "{parameter}: x-mcp-header does not form a valid HTTP header name"
        ));
    }
    let lower = header_name.to_ascii_lowercase();
    if !seen.insert(lower) {
        return Err(format!(
            "{parameter}: duplicate x-mcp-header value {header_name:?}"
        ));
    }
    let is_primitive = match schema.get("type") {
        Some(serde_json::Value::String(value)) => {
            matches!(value.as_str(), "string" | "number" | "integer" | "boolean")
        }
        Some(serde_json::Value::Array(values)) => values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|ty| matches!(ty, "string" | "number" | "integer" | "boolean"))
        }),
        _ => false,
    };
    if !is_primitive {
        return Err(format!(
            "{parameter}: x-mcp-header is only valid on primitive schema types"
        ));
    }
    Ok(())
}

fn encode_mcp_header_value(value: &serde_json::Value) -> Option<String> {
    let raw = match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    if is_plain_mcp_header_value(&raw) {
        Some(raw)
    } else {
        Some(format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
        ))
    }
}

fn is_plain_mcp_header_value(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| matches!(byte, b'\t' | b' '..=b'~'))
}

fn extract_content_text(result: &serde_json::Value) -> String {
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if texts.is_empty() {
            json_to_vm_value(result).display()
        } else {
            texts.join("\n")
        }
    } else {
        json_to_vm_value(result).display()
    }
}

/// Process-wide registry mapping MCP progress tokens to the agent
/// session that issued the originating `tools/call`. The MCP transport
/// reader (line-by-line stdio loop, SSE parser, multiplexed HTTP
/// stream) consults this registry when it sees an inbound
/// `notifications/progress` so the right session's `agent_inbox`
/// receives the update — even when the reader runs on a task that
/// doesn't share the issuer's thread-local current-session.
pub(crate) mod client_progress {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    use uuid::Uuid;

    /// Snapshot of an active progress-token registration.
    #[derive(Clone, Debug)]
    pub struct ProgressTokenContext {
        pub token: String,
        pub session_id: Option<String>,
        #[allow(dead_code)] // recorded for telemetry / future debug surface
        pub server: String,
        pub tool: String,
    }

    fn registry() -> &'static Mutex<HashMap<String, ProgressTokenContext>> {
        static REGISTRY: OnceLock<Mutex<HashMap<String, ProgressTokenContext>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Issue a fresh progress token for an outgoing `tools/call`. The
    /// token is registered against the active session id when one is
    /// available; calls made outside a session context still get a
    /// token (so the server emits notifications), but inbound updates
    /// for it have no inbox to land in and are dropped.
    pub fn issue_token(server: &str, tool: &str) -> Option<ProgressTokenContext> {
        let session_id = crate::llm::current_agent_session_id();
        let ctx = ProgressTokenContext {
            token: format!("hpt_{}", Uuid::now_v7()),
            session_id,
            server: server.to_string(),
            tool: tool.to_string(),
        };
        lock(registry()).insert(ctx.token.clone(), ctx.clone());
        Some(ctx)
    }

    /// Look up the registration for `token`. Returns `None` for unknown
    /// or already-released tokens (e.g. a late progress notification
    /// that arrived after the call completed).
    pub fn lookup(token: &str) -> Option<ProgressTokenContext> {
        lock(registry()).get(token).cloned()
    }

    /// Drop the registration for `token`. Idempotent.
    pub fn release(token: &str) {
        lock(registry()).remove(token);
    }

    /// RAII guard returned by [`issue_token`]; releases the
    /// registration on drop so a long-lived MCP connection doesn't
    /// accumulate stale tokens.
    pub struct ProgressTokenGuard {
        pub token: String,
    }

    impl Drop for ProgressTokenGuard {
        fn drop(&mut self) {
            release(&self.token);
        }
    }

    #[cfg(any(test, feature = "vm-bench-internals"))]
    #[allow(dead_code)]
    pub fn reset() {
        lock(registry()).clear();
    }
}

pub(crate) async fn call_mcp_tool(
    client: &VmMcpClientHandle,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    // Attach a unique progress token so the server can emit
    // `notifications/progress` updates that we route back into the
    // active session's `agent_inbox`. Tokens are scoped to the active
    // session id (when one is bound) and reaped on call completion via
    // the RAII `ProgressTokenGuard`.
    let progress_token = client_progress::issue_token(&client.name, tool_name);
    let _progress_guard = progress_token
        .as_ref()
        .map(|tok| client_progress::ProgressTokenGuard {
            token: tok.token.clone(),
        });
    let mut result = client
        .call(
            "tools/call",
            tool_call_params(tool_name, arguments.clone(), progress_token.as_ref(), None),
        )
        .await?;
    for _ in 0..MCP_INPUT_REQUIRED_MAX_ROUNDS {
        if result.get("resultType").and_then(|value| value.as_str())
            != Some(RESULT_TYPE_INPUT_REQUIRED)
        {
            break;
        }
        let Some(input_round) = resolve_input_required_result(&client.name, &result).await? else {
            break;
        };
        result = client
            .call(
                "tools/call",
                tool_call_params(
                    tool_name,
                    arguments.clone(),
                    progress_token.as_ref(),
                    Some(input_round),
                ),
            )
            .await?;
    }
    if result.get("resultType").and_then(|value| value.as_str()) == Some(RESULT_TYPE_INPUT_REQUIRED)
    {
        return Err(VmError::Runtime(format!(
            "MCP tool '{tool_name}' still required input after {MCP_INPUT_REQUIRED_MAX_ROUNDS} rounds"
        )));
    }

    if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
        let error_text = extract_content_text(&result);
        return Err(VmError::Thrown(VmValue::String(Rc::from(error_text))));
    }

    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    if content.len() == 1 && content[0].get("type").and_then(|t| t.as_str()) == Some("text") {
        if let Some(text) = content[0].get("text").and_then(|t| t.as_str()) {
            return Ok(serde_json::Value::String(text.to_string()));
        }
    }

    if content.is_empty() {
        Ok(serde_json::Value::Null)
    } else {
        Ok(serde_json::Value::Array(content))
    }
}

#[derive(Clone, Debug)]
struct McpInputRound {
    input_responses: serde_json::Value,
    request_state: Option<serde_json::Value>,
}

fn tool_call_params(
    tool_name: &str,
    arguments: serde_json::Value,
    progress_token: Option<&client_progress::ProgressTokenContext>,
    input_round: Option<McpInputRound>,
) -> serde_json::Value {
    let mut params = serde_json::Map::from_iter([
        (
            "name".to_string(),
            serde_json::Value::String(tool_name.to_string()),
        ),
        ("arguments".to_string(), arguments),
    ]);
    if let Some(tok) = progress_token {
        params.insert(
            "_meta".to_string(),
            serde_json::json!({ "progressToken": tok.token }),
        );
    }
    if let Some(input_round) = input_round {
        params.insert("inputResponses".to_string(), input_round.input_responses);
        if let Some(request_state) = input_round.request_state {
            params.insert("requestState".to_string(), request_state);
        }
    }
    serde_json::Value::Object(params)
}

async fn resolve_input_required_result(
    server_name: &str,
    result: &serde_json::Value,
) -> Result<Option<McpInputRound>, VmError> {
    let Some(input_requests) = result
        .get("inputRequests")
        .and_then(|value| value.as_object())
    else {
        return Ok(None);
    };
    let mut responses = serde_json::Map::new();
    for (key, input_request) in input_requests {
        let Some(method) = input_request.get("method").and_then(|value| value.as_str()) else {
            return Err(VmError::Runtime(format!(
                "MCP input_required request {key:?} is missing method"
            )));
        };
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": format!("input-{key}"),
            "method": method,
            "params": input_request
                .get("params")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        });
        let response = handle_inbound_client_request(server_name, &request)
            .await
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "MCP input_required request {key:?} used unsupported method {method:?}"
                ))
            })?;
        if let Some(result) = response.get("result") {
            responses.insert(key.clone(), result.clone());
        } else if let Some(error) = response.get("error") {
            return Err(jsonrpc_error_to_vm_error(error));
        } else {
            responses.insert(key.clone(), serde_json::Value::Null);
        }
    }
    Ok(Some(McpInputRound {
        input_responses: serde_json::Value::Object(responses),
        request_state: result.get("requestState").cloned(),
    }))
}

pub async fn connect_mcp_server_from_spec(
    spec: &McpServerSpec,
) -> Result<VmMcpClientHandle, VmError> {
    let mut handle = match spec.transport {
        McpTransport::Stdio => {
            let protocol_mode = resolve_protocol_mode(
                spec.protocol_mode.as_deref(),
                spec.protocol_version.as_deref(),
            )?;
            let protocol_version = spec
                .protocol_version
                .clone()
                .unwrap_or_else(|| default_protocol_version(protocol_mode).to_string());
            mcp_connect_stdio_impl(
                &spec.command,
                &spec.args,
                &spec.env,
                protocol_mode,
                protocol_version,
            )
            .await?
        }
        McpTransport::Http => mcp_connect_http_impl(spec).await?,
    };
    handle.name = spec.name.clone();
    Ok(handle)
}

pub async fn connect_mcp_server_from_json(
    value: &serde_json::Value,
) -> Result<VmMcpClientHandle, VmError> {
    let spec: McpServerSpec = serde_json::from_value(value.clone())
        .map_err(|e| VmError::Runtime(format!("Invalid MCP server config: {e}")))?;
    connect_mcp_server_from_spec(&spec).await
}

pub fn register_mcp_builtins(vm: &mut Vm) {
    vm.register_builtin("mcp_roots", mcp_roots_builtin);
    vm.register_builtin("harn.mcp.roots", mcp_roots_builtin);
    crate::mcp_file_upload::register_mcp_file_upload_builtins(vm);
    register_harn_mcp_namespace(vm);

    vm.register_async_builtin("mcp_connect", |args| async move {
        let command = args.first().map(|a| a.display()).unwrap_or_default();
        if command.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_connect: command is required",
            ))));
        }

        let cmd_args: Vec<String> = match args.get(1) {
            Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
            _ => Vec::new(),
        };

        let options = mcp_connect_options(args.get(2))?;
        let handle = mcp_connect_stdio_impl(
            &command,
            &cmd_args,
            &BTreeMap::new(),
            options.protocol_mode,
            options.protocol_version,
        )
        .await?;
        Ok(VmValue::mcp_client(handle))
    });

    // Lazy registry: ensure a registered server is booted and return its
    // live client handle. Used by skill activation (`requires_mcp`) and
    // by user code that wants to trigger a lazy connect explicitly.
    vm.register_async_builtin("mcp_ensure_active", |args| async move {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => String::new(),
        };
        if name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_ensure_active: server name is required",
            ))));
        }
        let handle = crate::mcp_registry::ensure_active(&name).await?;
        Ok(VmValue::mcp_client(handle))
    });

    // Decrement the binder refcount for a registered server. Called by
    // skill deactivation paths and by user code that manually bound via
    // `mcp_ensure_active`. No-op when the name isn't registered.
    vm.register_builtin("mcp_release", |args, _out| {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_release: server name is required",
                ))));
            }
        };
        crate::mcp_registry::release(&name);
        Ok(VmValue::Nil)
    });

    // Return the declared MCP servers and their current state as a list
    // of dicts. Purely diagnostic — useful for `harn` scripts that want
    // to show connection state in a status-line or dashboard.
    vm.register_builtin("mcp_registry_status", |_args, _out| {
        let mut out = Vec::new();
        for entry in crate::mcp_registry::snapshot_status() {
            let mut dict = BTreeMap::new();
            dict.insert(
                "name".to_string(),
                VmValue::String(Rc::from(entry.name.as_str())),
            );
            dict.insert("lazy".to_string(), VmValue::Bool(entry.lazy));
            dict.insert("active".to_string(), VmValue::Bool(entry.active));
            dict.insert(
                "ref_count".to_string(),
                VmValue::Int(entry.ref_count as i64),
            );
            if let Some(card) = entry.card {
                dict.insert("card".to_string(), VmValue::String(Rc::from(card.as_str())));
            }
            out.push(VmValue::Dict(Rc::new(dict)));
        }
        Ok(VmValue::List(Rc::new(out)))
    });

    // Fetch (or read from cache) the Server Card for a registered MCP
    // server, or from an explicit URL / local path.
    //
    // `mcp_server_card("notion")`           -> looks up `card = ...` in harn.toml
    // `mcp_server_card("https://.../card")` -> fetches that URL directly
    // `mcp_server_card("./card.json")`      -> reads that file directly
    vm.register_async_builtin("mcp_server_card", |args| async move {
        let target = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_server_card: server name, URL, or path is required",
                ))));
            }
        };

        // Source resolution: if the arg looks like a URL or path
        // (contains '/', '\\', or starts with a scheme), use it as-is.
        // Otherwise treat it as a registered server name and look up
        // its `card` field. This matches the user model: "I already
        // wrote down where the card lives in harn.toml — just use it."
        let source = if target.starts_with("http://")
            || target.starts_with("https://")
            || target.contains('/')
            || target.contains('\\')
            || target.ends_with(".json")
        {
            target.clone()
        } else {
            match crate::mcp_registry::get_registration(&target) {
                Some(reg) => match reg.card {
                    Some(card) => card,
                    None => {
                        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                            "mcp_server_card: server '{target}' has no 'card' field in harn.toml"
                        )))));
                    }
                },
                None => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "mcp_server_card: no MCP server '{target}' registered (check harn.toml) \
                         — pass a URL or path directly instead"
                    )))));
                }
            }
        };

        let card = crate::mcp_card::fetch_server_card(&source, None)
            .await
            .map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("mcp_server_card: {e}"))))
            })?;
        Ok(json_to_vm_value(&card))
    });

    vm.register_async_builtin("mcp_list_tools", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_tools: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("tools/list", serde_json::json!({})).await?;
        client.record_cache_hint("tools/list", &result).await;
        let mut tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        if client.protocol_mode().await? == McpProtocolMode::Modern {
            tools = filter_tools_for_client(&tools);
            client.store_http_tool_headers(&tools).await;
        }

        // Tag every tool with its originating server name so
        // downstream indexers (tool_search BM25) can surface them
        // under queries like "github" or "mcp:github". Harmless to
        // non-indexing callers — just an extra dict key.
        let server_name = client.name.clone();
        for tool in tools.iter_mut() {
            if let Some(obj) = tool.as_object_mut() {
                obj.entry("_mcp_server")
                    .or_insert_with(|| serde_json::Value::String(server_name.clone()));
            }
        }

        let vm_tools: Vec<VmValue> = tools.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_tools)))
    });

    vm.register_async_builtin("mcp_call", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_call: first argument must be an MCP client",
                ))));
            }
        };

        let tool_name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if tool_name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_call: tool name is required",
            ))));
        }

        let arguments = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            _ => serde_json::json!({}),
        };

        Ok(json_to_vm_value(
            &call_mcp_tool(&client, &tool_name, arguments).await?,
        ))
    });

    vm.register_async_builtin("mcp_server_info", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_server_info: argument must be an MCP client",
                ))));
            }
        };

        let guard = client.inner.lock().await;
        if guard.is_none() {
            return Err(VmError::Runtime("MCP client is disconnected".into()));
        }
        drop(guard);

        let mut info = BTreeMap::new();
        info.insert(
            "name".to_string(),
            VmValue::String(Rc::from(client.name.as_str())),
        );
        info.insert("connected".to_string(), VmValue::Bool(true));
        let initialize = client
            .initialize_result
            .lock()
            .await
            .clone()
            .unwrap_or(serde_json::Value::Null);
        if !initialize.is_null() {
            if let Some(instructions) = initialize
                .get("instructions")
                .or_else(|| {
                    initialize
                        .get("serverInfo")
                        .and_then(|value| value.get("instructions"))
                })
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
            {
                info.insert(
                    "instructions".to_string(),
                    VmValue::String(Rc::from(instructions)),
                );
            }
            info.insert("initialize".to_string(), json_to_vm_value(&initialize));
        }
        let cache_hints = client.cache_hints.lock().await;
        if !cache_hints.is_empty() {
            info.insert(
                "cache_hints".to_string(),
                json_to_vm_value(&cache_hints_to_json(cache_hints.iter())),
            );
        }
        Ok(VmValue::Dict(Rc::new(info)))
    });

    vm.register_async_builtin("mcp_disconnect", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_disconnect: argument must be an MCP client",
                ))));
            }
        };

        client.disconnect().await?;
        Ok(VmValue::Nil)
    });

    vm.register_async_builtin("mcp_list_resources", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_resources: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("resources/list", serde_json::json!({})).await?;
        client.record_cache_hint("resources/list", &result).await;
        let resources = result
            .get("resources")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_resources: Vec<VmValue> = resources.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_resources)))
    });

    vm.register_async_builtin("mcp_read_resource", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_read_resource: first argument must be an MCP client",
                ))));
            }
        };

        let uri = args.get(1).map(|a| a.display()).unwrap_or_default();
        if uri.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_read_resource: URI is required",
            ))));
        }

        let result = client
            .call("resources/read", serde_json::json!({ "uri": uri }))
            .await?;
        client.record_cache_hint("resources/read", &result).await;

        let contents = result
            .get("contents")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        if contents.len() == 1 {
            if let Some(text) = contents[0].get("text").and_then(|t| t.as_str()) {
                return Ok(VmValue::String(Rc::from(text)));
            }
        }

        if contents.is_empty() {
            Ok(VmValue::Nil)
        } else {
            Ok(VmValue::List(Rc::new(
                contents.iter().map(json_to_vm_value).collect(),
            )))
        }
    });

    vm.register_async_builtin("mcp_list_resource_templates", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_resource_templates: argument must be an MCP client",
                ))));
            }
        };

        let result = client
            .call("resources/templates/list", serde_json::json!({}))
            .await?;
        client
            .record_cache_hint("resources/templates/list", &result)
            .await;

        let templates = result
            .get("resourceTemplates")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_templates: Vec<VmValue> = templates.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_templates)))
    });

    vm.register_async_builtin("mcp_list_prompts", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_prompts: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("prompts/list", serde_json::json!({})).await?;
        client.record_cache_hint("prompts/list", &result).await;

        let prompts = result
            .get("prompts")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_prompts: Vec<VmValue> = prompts.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_prompts)))
    });

    vm.register_async_builtin("mcp_get_prompt", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_get_prompt: first argument must be an MCP client",
                ))));
            }
        };

        let name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_get_prompt: prompt name is required",
            ))));
        }

        let arguments = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            _ => serde_json::json!({}),
        };

        let result = client
            .call(
                "prompts/get",
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;

        Ok(json_to_vm_value(&result))
    });
}

fn mcp_roots_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(Rc::new(
        current_mcp_roots()
            .iter()
            .map(|root| json_to_vm_value(&root.script_json()))
            .collect(),
    )))
}

fn register_harn_mcp_namespace(vm: &mut Vm) {
    let mcp_namespace = VmValue::Dict(Rc::new(BTreeMap::from([
        (
            "_namespace".to_string(),
            VmValue::String(Rc::from("harn.mcp")),
        ),
        (
            "roots".to_string(),
            VmValue::BuiltinRef(Rc::from("harn.mcp.roots")),
        ),
        (
            "configure".to_string(),
            VmValue::BuiltinRef(Rc::from("harn.mcp.configure")),
        ),
        (
            "file_input".to_string(),
            VmValue::BuiltinRef(Rc::from("harn.mcp.file_input")),
        ),
        (
            "upload_file".to_string(),
            VmValue::BuiltinRef(Rc::from("harn.mcp.upload_file")),
        ),
    ])));
    vm.set_global(
        "harn",
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("_namespace".to_string(), VmValue::String(Rc::from("harn"))),
            (
                "mcp_roots".to_string(),
                VmValue::BuiltinRef(Rc::from("harn.mcp.roots")),
            ),
            ("mcp".to_string(), mcp_namespace),
        ]))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::framing::{http_content_length_from_headers, TEST_HTTP_MAX_BODY_BYTES};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;

    #[derive(Debug)]
    struct RecordedHttpRequest {
        headers: BTreeMap<String, String>,
        body: serde_json::Value,
    }

    // The loopback HTTP tests start background GET streams against in-process
    // mock servers. Keep those stream lifetimes isolated under the parallel
    // Rust test harness so one test cannot consume another test's scarce local
    // networking resources while it is still shutting down.
    async fn http_mcp_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_get_stream_dispatches_inbound_elicitation_response() {
        let _guard = http_mcp_test_guard().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                let (base_url, mut responses) = spawn_eliciting_http_mcp_server().await;
                let spec = McpServerSpec {
                    name: "mock-http".to_string(),
                    transport: McpTransport::Http,
                    command: String::new(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    url: format!("{base_url}/mcp"),
                    auth_token: None,
                    protocol_version: None,
                    protocol_mode: None,
                    proxy_server_name: None,
                };

                let handle = connect_mcp_server_from_spec(&spec).await.unwrap();
                let response = tokio::time::timeout(MCP_TIMEOUT, responses.recv())
                    .await
                    .expect("timed out waiting for elicitation response POST")
                    .expect("mock server closed before receiving elicitation response");

                assert_eq!(response["id"], serde_json::json!(99));
                assert_eq!(
                    response["result"]["action"],
                    serde_json::json!("decline"),
                    "without a host bridge, inbound elicitation should decline cleanly"
                );
                handle.disconnect().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_rc_connect_uses_server_discover_with_metadata() {
        let script = r#"
import json, sys
request = json.loads(sys.stdin.readline())
assert request["method"] == "server/discover"
meta = request["params"]["_meta"]
assert meta["io.modelcontextprotocol/protocolVersion"] == "DRAFT-2026-v1"
assert meta["io.modelcontextprotocol/clientInfo"]["name"] == "harn"
assert "io.modelcontextprotocol/clientCapabilities" in meta
print(json.dumps({
    "jsonrpc": "2.0",
    "id": request["id"],
    "result": {
        "resultType": "complete",
        "supportedVersions": ["DRAFT-2026-v1"],
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "modern", "version": "1.0.0"}
    }
}), flush=True)
"#;
        let handle = connect_stdio_test_script(
            script,
            McpProtocolMode::Modern,
            DRAFT_PROTOCOL_VERSION.to_string(),
        )
        .await;
        let initialize = handle.initialize_result.lock().await.clone().unwrap();
        assert_eq!(
            initialize["supportedVersions"],
            serde_json::json!([DRAFT_PROTOCOL_VERSION])
        );
        assert_eq!(
            handle.protocol_mode().await.unwrap(),
            McpProtocolMode::Modern
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_rc_connect_falls_back_to_initialize_only_on_method_not_found() {
        let script = r#"
import json, sys
discover = json.loads(sys.stdin.readline())
assert discover["method"] == "server/discover"
print(json.dumps({
    "jsonrpc": "2.0",
    "id": discover["id"],
    "error": {"code": -32601, "message": "Method not found"}
}), flush=True)
initialize = json.loads(sys.stdin.readline())
assert initialize["method"] == "initialize"
assert initialize["params"]["protocolVersion"] == "2025-11-25"
print(json.dumps({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "legacy", "version": "1.0.0"}
    }
}), flush=True)
initialized = json.loads(sys.stdin.readline())
assert initialized["method"] == "notifications/initialized"
"#;
        let handle = connect_stdio_test_script(
            script,
            McpProtocolMode::Modern,
            DRAFT_PROTOCOL_VERSION.to_string(),
        )
        .await;
        let initialize = handle.initialize_result.lock().await.clone().unwrap();
        assert_eq!(
            initialize["protocolVersion"],
            serde_json::json!(PROTOCOL_VERSION)
        );
        assert_eq!(
            handle.protocol_mode().await.unwrap(),
            McpProtocolMode::Legacy
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_rc_connect_retries_unsupported_protocol_version() {
        let script = r#"
import json, sys
first = json.loads(sys.stdin.readline())
assert first["method"] == "server/discover"
assert first["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] == "DRAFT-2026-v1"
print(json.dumps({
    "jsonrpc": "2.0",
    "id": first["id"],
    "error": {
        "code": -32004,
        "message": "Unsupported protocol version",
        "data": {"supported": ["2025-11-25"], "requested": "DRAFT-2026-v1"}
    }
}), flush=True)
second = json.loads(sys.stdin.readline())
assert second["method"] == "server/discover"
assert second["id"] != first["id"]
assert second["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] == "2025-11-25"
print(json.dumps({
    "jsonrpc": "2.0",
    "id": second["id"],
    "result": {
        "resultType": "complete",
        "supportedVersions": ["2025-11-25"],
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "modern-compat", "version": "1.0.0"}
    }
}), flush=True)
"#;
        let handle = connect_stdio_test_script(
            script,
            McpProtocolMode::Modern,
            DRAFT_PROTOCOL_VERSION.to_string(),
        )
        .await;
        assert_eq!(
            handle.protocol_mode().await.unwrap(),
            McpProtocolMode::Modern
        );
        assert_eq!(handle.protocol_version().await.unwrap(), PROTOCOL_VERSION);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn modern_http_sends_stateless_metadata_headers_and_schema_headers() {
        let _guard = http_mcp_test_guard().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                let (base_url, mut requests) = spawn_modern_http_mcp_server().await;
                let handle = modern_http_handle(&base_url).await;

                let tools_result = handle
                    .call("tools/list", serde_json::json!({}))
                    .await
                    .unwrap();
                handle.record_cache_hint("tools/list", &tools_result).await;
                let tools = filter_tools_for_client(
                    &tools_result["tools"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
                handle.store_http_tool_headers(&tools).await;
                assert_eq!(
                    handle.cache_hints.lock().await.get("tools/list"),
                    Some(&McpCacheHint {
                        ttl_ms: Some(300_000),
                        scope: Some("public"),
                    })
                );

                let call_result = call_mcp_tool(
                    &handle,
                    "execute_sql",
                    serde_json::json!({"region": "us-west1", "query": "select 1"}),
                )
                .await
                .unwrap();
                assert_eq!(call_result, serde_json::json!("ok"));

                let discover = recv_recorded_request(&mut requests).await;
                assert_modern_http_request(&discover, "server/discover", None);
                let list = recv_recorded_request(&mut requests).await;
                assert_modern_http_request(&list, "tools/list", None);
                let tool_call = recv_recorded_request(&mut requests).await;
                assert_modern_http_request(&tool_call, "tools/call", Some("execute_sql"));
                assert_eq!(
                    tool_call
                        .headers
                        .get("mcp-param-region")
                        .map(String::as_str),
                    Some("us-west1")
                );
                assert!(!tool_call.headers.contains_key("mcp-session-id"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn modern_http_discovery_falls_back_to_legacy_initialize_when_endpoint_is_not_modern() {
        let _guard = http_mcp_test_guard().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                let (base_url, mut requests) = spawn_legacy_http_fallback_server().await;
                let handle = modern_http_handle(&base_url).await;

                let discover = recv_recorded_request(&mut requests).await;
                assert_modern_http_request(&discover, "server/discover", None);
                let initialize = recv_recorded_request(&mut requests).await;
                assert_eq!(initialize.body["method"], serde_json::json!("initialize"));
                assert!(initialize.body["params"].get("_meta").is_none());
                assert_eq!(
                    initialize
                        .headers
                        .get("mcp-protocol-version")
                        .map(String::as_str),
                    Some(PROTOCOL_VERSION)
                );
                assert!(!initialize.headers.contains_key("mcp-method"));

                assert_eq!(
                    handle.protocol_mode().await.unwrap(),
                    McpProtocolMode::Legacy
                );
                let initialize_result = handle.initialize_result.lock().await.clone().unwrap();
                assert_eq!(
                    initialize_result["protocolVersion"],
                    serde_json::json!(PROTOCOL_VERSION)
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn modern_input_required_result_dispatches_and_retries() {
        let _guard = http_mcp_test_guard().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                let (base_url, mut requests) = spawn_modern_http_mcp_server().await;
                let handle = modern_http_handle(&base_url).await;
                install_sampling_mock().await;
                let result = call_mcp_tool(
                    &handle,
                    "needs_input",
                    serde_json::json!({"prompt": "continue"}),
                )
                .await
                .unwrap();
                assert_eq!(result, serde_json::json!("done"));

                let _discover = recv_recorded_request(&mut requests).await;
                let first_call = recv_recorded_request(&mut requests).await;
                assert_modern_http_request(&first_call, "tools/call", Some("needs_input"));
                assert!(first_call.body["params"].get("inputResponses").is_none());

                let retry_call = recv_recorded_request(&mut requests).await;
                assert_modern_http_request(&retry_call, "tools/call", Some("needs_input"));
                let responses = &retry_call.body["params"]["inputResponses"];
                assert!(responses["roots"]["roots"].as_array().is_some());
                assert_eq!(
                    responses["elicitation"]["action"],
                    serde_json::json!("decline")
                );
                assert_eq!(
                    responses["sampling"]["content"]["text"],
                    serde_json::json!("sampled")
                );
                assert_eq!(
                    retry_call.body["params"]["requestState"],
                    serde_json::json!("state-1")
                );
                clear_sampling_mock().await;
            })
            .await;
    }

    #[test]
    fn x_mcp_header_validation_filters_invalid_tools_and_encodes_values() {
        let tools = vec![
            serde_json::json!({
                "name": "valid",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "region": {"type": "string", "x-mcp-header": "Region"}
                    }
                }
            }),
            serde_json::json!({
                "name": "invalid",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "body": {"type": "object", "x-mcp-header": "Body"}
                    }
                }
            }),
        ];
        let filtered = filter_tools_for_client(&tools);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["name"], serde_json::json!("valid"));
        assert_eq!(
            encode_mcp_header_value(&serde_json::json!("Hello, 世界")).unwrap(),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
    }

    async fn connect_stdio_test_script(
        script: &str,
        protocol_mode: McpProtocolMode,
        protocol_version: String,
    ) -> VmMcpClientHandle {
        let args = vec!["-u".to_string(), "-c".to_string(), script.to_string()];
        mcp_connect_stdio_impl(
            "python3",
            &args,
            &BTreeMap::new(),
            protocol_mode,
            protocol_version,
        )
        .await
        .expect("stdio test MCP server should connect")
    }

    async fn install_sampling_mock() {
        execute_test_harn(
            r#"
llm_mock({text: "sampled", provider: "mock", model: "mock"})
host_mock("mcp", "sample", {action: "accept", options: {provider: "mock", model: "mock"}})
"#,
        )
        .await;
    }

    async fn clear_sampling_mock() {
        execute_test_harn(
            r#"
host_mock_clear()
llm_mock_clear()
"#,
        )
        .await;
    }

    async fn execute_test_harn(source: &str) {
        let chunk = crate::compile_source(source).expect("test Harn source should compile");
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib_with_deferred_llm(&mut vm);
        vm.execute(&chunk)
            .await
            .expect("test Harn source should execute");
    }

    async fn modern_http_handle(base_url: &str) -> VmMcpClientHandle {
        let spec = McpServerSpec {
            name: "modern-http".to_string(),
            transport: McpTransport::Http,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: format!("{base_url}/mcp"),
            auth_token: None,
            protocol_version: Some(DRAFT_PROTOCOL_VERSION.to_string()),
            protocol_mode: Some("rc".to_string()),
            proxy_server_name: None,
        };
        connect_mcp_server_from_spec(&spec)
            .await
            .expect("modern HTTP MCP server should connect")
    }

    fn assert_modern_http_request(request: &RecordedHttpRequest, method: &str, name: Option<&str>) {
        assert_eq!(request.body["method"], serde_json::json!(method));
        assert_eq!(
            request
                .headers
                .get("mcp-protocol-version")
                .map(String::as_str),
            Some(DRAFT_PROTOCOL_VERSION)
        );
        assert_eq!(
            request.headers.get("mcp-method").map(String::as_str),
            Some(method)
        );
        assert_eq!(request.headers.get("mcp-name").map(String::as_str), name);
        assert!(!request.headers.contains_key("mcp-session-id"));
        let meta = &request.body["params"]["_meta"];
        assert_eq!(
            meta[RC_META_KEY_PROTOCOL_VERSION],
            serde_json::json!(DRAFT_PROTOCOL_VERSION)
        );
        assert_eq!(
            meta[RC_META_KEY_CLIENT_INFO]["name"],
            serde_json::json!("harn")
        );
        assert_eq!(
            meta[RC_META_KEY_CLIENT_CAPABILITIES]["roots"],
            serde_json::json!({})
        );
    }

    async fn spawn_modern_http_mcp_server() -> (String, mpsc::UnboundedReceiver<RecordedHttpRequest>)
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let Ok((_request_line, headers, body)) = read_http_request(&mut stream).await
                else {
                    continue;
                };
                let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                    continue;
                };
                let _ = request_tx.send(RecordedHttpRequest {
                    headers: headers.clone(),
                    body: request.clone(),
                });
                let method = request.get("method").and_then(|value| value.as_str());
                let response = modern_http_response(&request, method);
                let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
            }
        });

        (format!("http://{addr}"), request_rx)
    }

    async fn spawn_legacy_http_fallback_server(
    ) -> (String, mpsc::UnboundedReceiver<RecordedHttpRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let Ok((request_line, headers, body)) = read_http_request(&mut stream).await else {
                    continue;
                };
                if request_line.starts_with("GET ") {
                    let _ = write_http_empty(&mut stream, "404 Not Found").await;
                    continue;
                }
                let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                    continue;
                };
                let method = request.get("method").and_then(|value| value.as_str());
                let _ = request_tx.send(RecordedHttpRequest {
                    headers: headers.clone(),
                    body: request.clone(),
                });
                match method {
                    Some("server/discover") => {
                        let _ = write_http_empty(&mut stream, "400 Bad Request").await;
                    }
                    Some("initialize") => {
                        let response = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"].clone(),
                            "result": {
                                "protocolVersion": PROTOCOL_VERSION,
                                "capabilities": {"tools": {}},
                                "serverInfo": {"name": "legacy-http", "version": "1.0.0"}
                            }
                        });
                        let _ = write_http_json(
                            &mut stream,
                            "200 OK",
                            &[("MCP-Session-Id", "legacy-session")],
                            response,
                        )
                        .await;
                    }
                    Some("notifications/initialized") => {
                        let _ = write_http_empty(&mut stream, "202 Accepted").await;
                    }
                    _ => {
                        let _ = write_http_empty(&mut stream, "404 Not Found").await;
                    }
                }
            }
        });

        (format!("http://{addr}"), request_rx)
    }

    async fn recv_recorded_request(
        requests: &mut mpsc::UnboundedReceiver<RecordedHttpRequest>,
    ) -> RecordedHttpRequest {
        tokio::time::timeout(MCP_TIMEOUT, requests.recv())
            .await
            .expect("timed out waiting for recorded MCP HTTP request")
            .expect("mock server closed before recording request")
    }

    fn modern_http_response(
        request: &serde_json::Value,
        method: Option<&str>,
    ) -> serde_json::Value {
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match method {
            Some("server/discover") => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "resultType": "complete",
                    "supportedVersions": [DRAFT_PROTOCOL_VERSION],
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "modern-http", "version": "1.0.0"}
                }
            }),
            Some("tools/list") => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "resultType": "complete",
                    "tools": [{
                        "name": "execute_sql",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "region": {"type": "string", "x-mcp-header": "Region"},
                                "query": {"type": "string"}
                            },
                            "required": ["region", "query"]
                        }
                    }],
                    "ttlMs": 300000,
                    "cacheScope": "public"
                }
            }),
            Some("tools/call") => modern_http_tool_call_response(request, id),
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "Method not found"}
            }),
        }
    }

    fn modern_http_tool_call_response(
        request: &serde_json::Value,
        id: serde_json::Value,
    ) -> serde_json::Value {
        let params = &request["params"];
        let name = params.get("name").and_then(|value| value.as_str());
        if name == Some("needs_input") && params.get("inputResponses").is_none() {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "resultType": "input_required",
                    "requestState": "state-1",
                    "inputRequests": {
                        "roots": {"method": "roots/list", "params": {}},
                        "elicitation": {
                            "method": "elicitation/create",
                            "params": {
                                "mode": "form",
                                "message": "Need input",
                                "requestedSchema": {
                                    "type": "object",
                                    "properties": {"answer": {"type": "string"}}
                                }
                            }
                        },
                        "sampling": {
                            "method": "sampling/createMessage",
                            "params": {
                                "messages": [{
                                    "role": "user",
                                    "content": {"type": "text", "text": "sample"}
                                }],
                                "maxTokens": 4
                            }
                        }
                    }
                }
            });
        }
        let text = if name == Some("needs_input") {
            "done"
        } else {
            "ok"
        };
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "content": [{
                    "type": "text",
                    "text": text
                }],
                "isError": false
            }
        })
    }

    async fn spawn_eliciting_http_mcp_server(
    ) -> (String, mpsc::UnboundedReceiver<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (response_tx, response_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let _ = handle_mock_http_mcp_connection(stream, response_tx.clone()).await;
            }
        });

        (format!("http://{addr}"), response_rx)
    }

    async fn spawn_recording_http_mcp_server(
    ) -> (String, mpsc::UnboundedReceiver<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let Ok((_request_line, _headers, body)) = read_http_request(&mut stream).await
                else {
                    continue;
                };
                if let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) {
                    let _ = request_tx.send(request);
                }
                let _ = write_http_empty(&mut stream, "202 Accepted").await;
            }
        });

        (format!("http://{addr}"), request_rx)
    }

    async fn handle_mock_http_mcp_connection(
        mut stream: TcpStream,
        response_tx: mpsc::UnboundedSender<serde_json::Value>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (request_line, headers, body) = read_http_request(&mut stream).await?;
        if request_line.starts_with("GET ") {
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "content-type: text/event-stream\r\n",
                "cache-control: no-cache\r\n",
                "connection: close\r\n",
                "\r\n",
                "id: prime\r\n",
                "data: \r\n",
                "\r\n",
                "id: elicit-1\r\n",
                "event: message\r\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"elicitation/create\",\"params\":{\"message\":\"Need input\",\"requestedSchema\":{\"type\":\"object\",\"properties\":{}}}}\r\n",
                "\r\n"
            );
            stream.write_all(response.as_bytes()).await?;
            stream.flush().await?;
            return Ok(());
        }

        let request: serde_json::Value = serde_json::from_slice(&body)?;
        let method = request.get("method").and_then(|value| value.as_str());
        match method {
            Some("initialize") => {
                write_http_json(
                    &mut stream,
                    "200 OK",
                    &[("MCP-Session-Id", "test-session")],
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"].clone(),
                        "result": {
                            "protocolVersion": PROTOCOL_VERSION,
                            "capabilities": {
                                "elicitation": {},
                                "tools": {}
                            },
                            "serverInfo": {
                                "name": "mock",
                                "version": "0.0.0"
                            }
                        }
                    }),
                )
                .await?;
            }
            Some("notifications/initialized") => {
                write_http_empty(&mut stream, "202 Accepted").await?;
            }
            _ if request.get("result").is_some() || request.get("error").is_some() => {
                assert_eq!(
                    headers.get("mcp-session-id").map(String::as_str),
                    Some("test-session")
                );
                let _ = response_tx.send(request);
                write_http_empty(&mut stream, "202 Accepted").await?;
            }
            _ => {
                write_http_json(
                    &mut stream,
                    "200 OK",
                    &[],
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"].clone(),
                        "result": {}
                    }),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn read_http_request(
        stream: &mut TcpStream,
    ) -> Result<(String, BTreeMap<String, String>, Vec<u8>), Box<dyn std::error::Error + Send + Sync>>
    {
        let mut buffer = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let bytes = stream.read(&mut chunk).await?;
            if bytes == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let header_end = buffer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or("missing HTTP header terminator")?;
        let header_text = String::from_utf8(buffer[..header_end].to_vec())?;
        let mut lines = header_text.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let mut headers = BTreeMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let content_length = http_content_length_from_headers(&headers, TEST_HTTP_MAX_BODY_BYTES)?;
        let mut body = buffer[header_end + 4..].to_vec();
        let mut chunk = [0_u8; 8192];
        while body.len() < content_length {
            let remaining = content_length - body.len();
            let read_len = remaining.min(chunk.len());
            let bytes = stream.read(&mut chunk[..read_len]).await?;
            if bytes == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..bytes]);
        }
        body.truncate(content_length);
        Ok((request_line, headers, body))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_http_request_rejects_oversized_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.expect("connect");
            let request = format!(
                "POST /mcp HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
                TEST_HTTP_MAX_BODY_BYTES + 1
            );
            stream.write_all(request.as_bytes()).await.expect("write");
        });

        let (mut stream, _) = listener.accept().await.expect("accept");
        let error = read_http_request(&mut stream)
            .await
            .expect_err("oversized content length should be rejected");
        assert!(error.to_string().contains("exceeds limit"));
        client.await.expect("client task");
    }

    async fn write_http_json(
        stream: &mut TcpStream,
        status: &str,
        headers: &[(&str, &str)],
        body: serde_json::Value,
    ) -> Result<(), std::io::Error> {
        let body = serde_json::to_string(&body).unwrap();
        let mut response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(&body);
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await
    }

    async fn write_http_empty(stream: &mut TcpStream, status: &str) -> Result<(), std::io::Error> {
        let response =
            format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await
    }

    #[test]
    fn test_vm_value_to_serde_string() {
        let val = VmValue::String(Rc::from("hello"));
        let json = vm_value_to_serde(&val);
        assert_eq!(json, serde_json::json!("hello"));
    }

    #[test]
    fn test_vm_value_to_serde_dict() {
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), VmValue::Int(42));
        let val = VmValue::Dict(Rc::new(map));
        let json = vm_value_to_serde(&val);
        assert_eq!(json, serde_json::json!({"key": 42}));
    }

    #[test]
    fn test_vm_value_to_serde_list() {
        let val = VmValue::List(Rc::new(vec![VmValue::Int(1), VmValue::Int(2)]));
        let json = vm_value_to_serde(&val);
        assert_eq!(json, serde_json::json!([1, 2]));
    }

    #[test]
    fn test_extract_content_text_single() {
        let result = serde_json::json!({
            "content": [{"type": "text", "text": "hello world"}],
            "isError": false
        });
        assert_eq!(extract_content_text(&result), "hello world");
    }

    #[test]
    fn test_extract_content_text_multiple() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ],
            "isError": false
        });
        assert_eq!(extract_content_text(&result), "first\nsecond");
    }

    #[test]
    fn test_extract_content_text_fallback_json() {
        let result = serde_json::json!({
            "content": [{"type": "image", "data": "abc"}],
            "isError": false
        });
        let output = extract_content_text(&result);
        assert!(output.contains("image"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_parse_sse_jsonrpc_body_uses_matching_jsonrpc_response() {
        let inner = HttpMcpClientInner {
            client: reqwest::Client::new(),
            url: "http://127.0.0.1/mcp".to_string(),
            auth_token: None,
            protocol_mode: McpProtocolMode::Legacy,
            protocol_version: PROTOCOL_VERSION.to_string(),
            session_id: None,
            next_id: 1,
            proxy_server_name: None,
            get_stream_task: None,
            tool_headers: BTreeMap::new(),
        };
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let parsed = parse_sse_jsonrpc_body(&inner, "mock", body, Some(1))
            .await
            .unwrap();
        assert_eq!(parsed["result"]["tools"], serde_json::json!([]));
    }

    #[test]
    fn client_rejects_unadvertised_server_to_client_requests() {
        let unknown = client_request_rejection(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "custom-1",
            "method": "custom/method",
            "params": {}
        }))
        .expect("rejection");
        assert_eq!(unknown["error"]["code"], serde_json::json!(-32601));
        assert!(unknown["error"].get("data").is_none());
    }

    #[test]
    fn current_mcp_roots_prefers_project_root_over_child_cwd() {
        let root = std::env::temp_dir().join(format!("harn-mcp-roots-{}", uuid::Uuid::now_v7()));
        let child = root.join("nested");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(root.join("harn.toml"), "[package]\nname = \"roots\"\n").unwrap();

        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(child.to_string_lossy().into_owned()),
                source_dir: Some(child.to_string_lossy().into_owned()),
                ..Default::default()
            },
        ));

        let roots = current_mcp_roots();
        let expected_root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, expected_root.to_string_lossy());
        assert!(roots[0].uri.starts_with("file://"));
        assert_eq!(
            roots[0].name,
            expected_root.file_name().unwrap().to_string_lossy()
        );

        crate::stdlib::process::reset_process_state();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_inbound_routes_roots_list() {
        let root = std::env::temp_dir().join(format!("harn-mcp-roots-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(root.to_string_lossy().into_owned()),
                ..Default::default()
            },
        ));

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "roots-1",
            "method": crate::mcp_protocol::METHOD_ROOTS_LIST,
        });
        let response = handle_inbound_client_request("mock", &request)
            .await
            .expect("roots/list should produce a response");
        let expected_root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(response["id"], serde_json::json!("roots-1"));
        assert_eq!(response["result"]["roots"].as_array().unwrap().len(), 1);
        assert_eq!(
            response["result"]["roots"][0]["uri"],
            serde_json::json!(url::Url::from_file_path(&expected_root)
                .unwrap()
                .to_string())
        );

        crate::stdlib::process::reset_process_state();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn roots_list_changed_notification_is_sent_once_per_snapshot() {
        let _guard = http_mcp_test_guard().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                let (base_url, mut requests) = spawn_recording_http_mcp_server().await;
                let handle = VmMcpClientHandle {
                    name: "mock-http".to_string(),
                    inner: Arc::new(Mutex::new(Some(McpClientInner::Http(HttpMcpClientInner {
                        client: reqwest::Client::new(),
                        url: format!("{base_url}/mcp"),
                        auth_token: None,
                        protocol_mode: McpProtocolMode::Legacy,
                        protocol_version: PROTOCOL_VERSION.to_string(),
                        session_id: None,
                        next_id: 1,
                        proxy_server_name: None,
                        get_stream_task: None,
                        tool_headers: BTreeMap::new(),
                    })))),
                    last_roots: Arc::new(Mutex::new(Vec::new())),
                    initialize_result: Arc::new(Mutex::new(None)),
                    cache_hints: Arc::new(Mutex::new(BTreeMap::new())),
                };

                handle.notify_roots_list_changed_if_needed().await.unwrap();
                let notification = tokio::time::timeout(MCP_TIMEOUT, requests.recv())
                    .await
                    .expect("timed out waiting for roots notification")
                    .expect("mock server closed before notification");
                assert_eq!(
                    notification["method"],
                    serde_json::json!(crate::mcp_protocol::METHOD_ROOTS_LIST_CHANGED_NOTIFICATION)
                );

                handle.notify_roots_list_changed_if_needed().await.unwrap();
                assert!(requests.try_recv().is_err());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_inbound_routes_sampling_to_dispatcher() {
        // Confirms `sampling/createMessage` is routed to
        // `mcp_sampling::dispatch_inbound_sampling` rather than the
        // generic rejection path. With no host bridge installed, the
        // dispatcher declines with the structured `mcp.samplingDeclined`
        // error envelope — proving the request reached the right
        // handler instead of being bounced as `Method not found`.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": crate::mcp_sampling::SAMPLING_METHOD,
            "params": {
                "messages": [
                    {"role": "user", "content": {"type": "text", "text": "ping"}}
                ],
                "maxTokens": 4,
            },
        });
        let response = handle_inbound_client_request("mock", &request)
            .await
            .expect("sampling should produce a response");
        assert_eq!(response["id"], serde_json::json!(42));
        assert_eq!(
            response["error"]["data"]["type"],
            serde_json::json!("mcp.samplingDeclined")
        );
    }
}
