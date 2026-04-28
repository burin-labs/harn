//! MCP (Model Context Protocol) client for connecting to external tool servers.
//!
//! Supports stdio transport and streamable HTTP-style request/response transport.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// MCP protocol version we negotiate by default.
const PROTOCOL_VERSION: &str = "2025-11-25";

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
}

struct HttpMcpClientInner {
    client: reqwest::Client,
    url: String,
    auth_token: Option<String>,
    protocol_version: String,
    session_id: Option<String>,
    next_id: u64,
    proxy_server_name: Option<String>,
}

/// Handle to an MCP client connection, stored in VmValue.
#[derive(Clone)]
pub struct VmMcpClientHandle {
    pub name: String,
    inner: Arc<Mutex<Option<McpClientInner>>>,
}

impl std::fmt::Debug for VmMcpClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "McpClient({})", self.name)
    }
}

impl VmMcpClientHandle {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        match inner {
            McpClientInner::Stdio(inner) => stdio_call(inner, method, params).await,
            McpClientInner::Http(inner) => http_call(inner, method, params).await,
        }
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), VmError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        match inner {
            McpClientInner::Stdio(inner) => stdio_notify(inner, method, params).await,
            McpClientInner::Http(inner) => http_notify(inner, method, params).await,
        }
    }
}

async fn stdio_call(
    inner: &mut StdioMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let id = inner.next_id;
    inner.next_id += 1;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });

    let line = serde_json::to_string(&request)
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
            continue;
        }

        if msg["id"].as_u64() == Some(id)
            && (msg.get("result").is_some() || msg.get("error").is_some())
        {
            return parse_jsonrpc_result(msg);
        }

        if let Some(response) = client_request_rejection(&msg) {
            let line = serde_json::to_string(&response)
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
        }
    }
}

async fn stdio_notify(
    inner: &mut StdioMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<(), VmError> {
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
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

async fn http_call(
    inner: &mut HttpMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let id = inner.next_id;
    inner.next_id += 1;
    send_http_request(inner, method, params, Some(id)).await
}

async fn http_notify(
    inner: &mut HttpMcpClientInner,
    method: &str,
    params: serde_json::Value,
) -> Result<(), VmError> {
    let _ = send_http_request(inner, method, params, None).await?;
    Ok(())
}

async fn send_http_request(
    inner: &mut HttpMcpClientInner,
    method: &str,
    params: serde_json::Value,
    id: Option<u64>,
) -> Result<serde_json::Value, VmError> {
    for attempt in 0..2 {
        let response = send_http_request_once(inner, method, params.clone(), id).await?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if let Some(protocol_version) = headers
            .get("MCP-Protocol-Version")
            .and_then(|v| v.to_str().ok())
        {
            inner.protocol_version = protocol_version.to_string();
        }
        if let Some(session_id) = headers.get("MCP-Session-Id").and_then(|v| v.to_str().ok()) {
            inner.session_id = Some(session_id.to_string());
        }

        if status == 404 && inner.session_id.is_some() && method != "initialize" && attempt == 0 {
            inner.session_id = None;
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
            return Ok(serde_json::Value::Null);
        }

        let msg = parse_http_response_body(&body, status)?;

        if status >= 400 {
            return Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)));
        }

        if id.is_none() {
            return Ok(msg);
        }
        return parse_jsonrpc_result(msg);
    }

    Err(VmError::Runtime("MCP HTTP request failed".into()))
}

async fn send_http_request_once(
    inner: &mut HttpMcpClientInner,
    method: &str,
    params: serde_json::Value,
    id: Option<u64>,
) -> Result<reqwest::Response, VmError> {
    let payload = if let Some(proxy_server_name) = &inner.proxy_server_name {
        let mut body = serde_json::json!({
            "serverName": proxy_server_name,
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if let Some(id) = id {
            body["id"] = serde_json::json!(id);
        }
        body
    } else {
        let mut body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if let Some(id) = id {
            body["id"] = serde_json::json!(id);
        }
        body
    };

    let mut request = inner
        .client
        .post(&inner.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", &inner.protocol_version)
        .json(&payload);

    if let Some(token) = &inner.auth_token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(session_id) = &inner.session_id {
        request = request.header("MCP-Session-Id", session_id);
    }

    request
        .send()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP request error: {e}")))
}

async fn reinitialize_http_client(inner: &mut HttpMcpClientInner) -> Result<(), VmError> {
    let initialize = send_http_request_once(
        inner,
        "initialize",
        serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "harn",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
        Some(0),
    )
    .await?;
    if let Some(protocol_version) = initialize
        .headers()
        .get("MCP-Protocol-Version")
        .and_then(|v| v.to_str().ok())
    {
        inner.protocol_version = protocol_version.to_string();
    }
    if let Some(session_id) = initialize
        .headers()
        .get("MCP-Session-Id")
        .and_then(|v| v.to_str().ok())
    {
        inner.session_id = Some(session_id.to_string());
    }
    let status = initialize.status().as_u16();
    let body = initialize
        .text()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP read error: {e}")))?;
    let msg = parse_http_response_body(&body, status)?;
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
        .get("MCP-Protocol-Version")
        .and_then(|v| v.to_str().ok())
    {
        inner.protocol_version = protocol_version.to_string();
    }
    if let Some(session_id) = response
        .headers()
        .get("MCP-Session-Id")
        .and_then(|v| v.to_str().ok())
    {
        inner.session_id = Some(session_id.to_string());
    }
    let body = response
        .text()
        .await
        .map_err(|e| VmError::Runtime(format!("MCP HTTP read error: {e}")))?;
    if body.trim().is_empty() || status < 400 {
        return Ok(());
    }
    let msg = parse_http_response_body(&body, status)?;
    Err(jsonrpc_error_to_vm_error(msg.get("error").unwrap_or(&msg)))
}

fn parse_http_response_body(body: &str, status: u16) -> Result<serde_json::Value, VmError> {
    if body.trim_start().starts_with("event:") || body.trim_start().starts_with("data:") {
        return parse_sse_jsonrpc_body(body);
    }
    serde_json::from_str(body).map_err(|e| {
        VmError::Runtime(format!(
            "MCP HTTP response parse error (status {status}): {e}"
        ))
    })
}

fn parse_sse_jsonrpc_body(body: &str) -> Result<serde_json::Value, VmError> {
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

    for message in messages.into_iter().rev() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message) {
            if value.get("result").is_some()
                || value.get("error").is_some()
                || value.get("method").is_some()
            {
                return Ok(value);
            }
        }
    }

    Err(VmError::Runtime(
        "MCP HTTP response parse error: no JSON-RPC payload found in SSE stream".into(),
    ))
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
    crate::mcp_protocol::unsupported_latest_spec_method_response(request_id.clone(), method)
        .or_else(|| {
            Some(crate::jsonrpc::error_response(
                request_id,
                -32601,
                &format!("Method not found: {method}"),
            ))
        })
}

async fn mcp_connect_stdio_impl(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<VmMcpClientHandle, VmError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .envs(env);

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
            },
        )))),
    };

    initialize_client(&handle).await?;
    Ok(handle)
}

async fn mcp_connect_http_impl(spec: &McpServerSpec) -> Result<VmMcpClientHandle, VmError> {
    let client = reqwest::Client::builder()
        .timeout(MCP_TIMEOUT)
        .build()
        .map_err(|e| VmError::Runtime(format!("MCP HTTP client error: {e}")))?;

    let handle = VmMcpClientHandle {
        name: spec.name.clone(),
        inner: Arc::new(Mutex::new(Some(McpClientInner::Http(HttpMcpClientInner {
            client,
            url: spec.url.clone(),
            auth_token: spec.auth_token.clone(),
            protocol_version: spec
                .protocol_version
                .clone()
                .unwrap_or_else(|| PROTOCOL_VERSION.to_string()),
            session_id: None,
            next_id: 1,
            proxy_server_name: spec.proxy_server_name.clone(),
        })))),
    };

    initialize_client(&handle).await?;
    Ok(handle)
}

async fn initialize_client(handle: &VmMcpClientHandle) -> Result<(), VmError> {
    handle
        .call(
            "initialize",
            serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
        .await?;

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

pub async fn connect_mcp_server(
    name: &str,
    command: &str,
    args: &[String],
) -> Result<VmMcpClientHandle, VmError> {
    let mut handle = mcp_connect_stdio_impl(command, args, &BTreeMap::new()).await?;
    handle.name = name.to_string();
    Ok(handle)
}

pub async fn connect_mcp_server_from_spec(
    spec: &McpServerSpec,
) -> Result<VmMcpClientHandle, VmError> {
    let mut handle = match spec.transport {
        McpTransport::Stdio => mcp_connect_stdio_impl(&spec.command, &spec.args, &spec.env).await?,
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

        let handle = mcp_connect_stdio_impl(&command, &cmd_args, &BTreeMap::new()).await?;
        Ok(VmValue::McpClient(handle))
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
        Ok(VmValue::McpClient(handle))
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
        let mut tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

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

        let result = client
            .call(
                "tools/call",
                serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await?;

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
                return Ok(VmValue::String(Rc::from(text)));
            }
        }

        if content.is_empty() {
            Ok(VmValue::Nil)
        } else {
            Ok(VmValue::List(Rc::new(
                content.iter().map(json_to_vm_value).collect(),
            )))
        }
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

        let mut guard = client.inner.lock().await;
        if let Some(inner) = guard.take() {
            match inner {
                McpClientInner::Stdio(mut inner) => {
                    let _ = inner.child.kill().await;
                }
                McpClientInner::Http(_) => {}
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_parse_sse_jsonrpc_body_uses_last_jsonrpc_message() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let parsed = parse_sse_jsonrpc_body(body).unwrap();
        assert_eq!(parsed["result"]["tools"], serde_json::json!([]));
    }

    #[test]
    fn client_rejects_unadvertised_server_to_client_requests() {
        let roots = client_request_rejection(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "roots-1",
            "method": "roots/list",
            "params": {}
        }))
        .expect("rejection");
        assert_eq!(roots["error"]["code"], serde_json::json!(-32601));
        assert_eq!(
            roots["error"]["data"]["feature"],
            serde_json::json!("roots")
        );

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
}
