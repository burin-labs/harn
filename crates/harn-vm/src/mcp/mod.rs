//! MCP (Model Context Protocol) client for connecting to external tool servers.
//!
//! Supports stdio transport and streamable HTTP-style request/response transport.

pub(crate) use std::collections::BTreeMap;
pub(crate) use std::future::Future;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::Arc;

pub(crate) use base64::Engine;
pub(crate) use futures::StreamExt;
pub(crate) use reqwest_eventsource::{Event as SseEvent, EventSource};
pub(crate) use serde::Deserialize;
pub(crate) use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
pub(crate) use tokio::process::{Child, ChildStdin, ChildStdout};
pub(crate) use tokio::sync::Mutex;

pub(crate) use crate::stdlib::json_to_vm_value;
pub(crate) use crate::value::{VmError, VmValue};
pub(crate) use crate::vm::Vm;

pub(crate) use crate::mcp_protocol::{
    cache_hints_to_json, rc_name_header_value, McpCacheHint, McpProtocolMode,
    DRAFT_PROTOCOL_VERSION, LEGACY_2025_06_18_PROTOCOL_VERSION, MCP_SESSION_HEADER_LEGACY,
    PROTOCOL_VERSION, RC_HEADER_METHOD, RC_HEADER_NAME, RC_HEADER_PROTOCOL_VERSION,
    RC_META_KEY_CLIENT_CAPABILITIES, RC_META_KEY_CLIENT_INFO, RC_META_KEY_PROTOCOL_VERSION,
    RESULT_TYPE_INPUT_REQUIRED, UNSUPPORTED_PROTOCOL_VERSION_CODE,
};

mod builtins;
mod connect;
mod notifications;
mod protocol;
mod roots;
mod transport;

pub use builtins::*;
pub use connect::*;
pub(crate) use notifications::*;
pub(crate) use protocol::*;
pub(crate) use roots::*;
pub(crate) use transport::*;

const X_MCP_HEADER: &str = "x-mcp-header";

const MCP_INPUT_REQUIRED_MAX_ROUNDS: usize = 8;

/// Default timeout for MCP requests (60 seconds).
///
/// For a stdio request this is the **cumulative** budget for the entire
/// response wait, not a per-read timeout. A server that interleaves
/// notifications (or dribbles output) can no longer keep an agent turn open
/// indefinitely by resetting a per-read clock; a single synchronous MCP call
/// is bounded end to end, and genuinely long-running work must use the
/// task/progress-polling pattern rather than a call held open for minutes
/// (harn#4390).
const MCP_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum McpTransport {
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
    pub token_exchange: Option<crate::mcp_oauth::McpTokenExchangeConfig>,
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
pub(crate) enum McpClientInner {
    Stdio(StdioMcpClientInner),
    Http(HttpMcpClientInner),
}

pub(crate) struct StdioMcpClientInner {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
    /// Cumulative budget for one request's response wait; defaults to
    /// [`MCP_TIMEOUT`]. A field (rather than a bare use of the constant)
    /// keeps the deadline injectable so tests can prove the cumulative bound
    /// without waiting the full production budget (harn#4390).
    response_deadline: std::time::Duration,
}

pub(crate) struct HttpMcpClientInner {
    client: reqwest::Client,
    url: String,
    auth_token: Option<String>,
    auth_token_source: HttpAuthTokenSource,
    token_exchange: Option<Arc<crate::mcp_oauth::McpTokenExchangeConfig>>,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
    session_id: Option<String>,
    next_id: u64,
    proxy_server_name: Option<String>,
    get_stream_task: Option<tokio::task::JoinHandle<()>>,
    tool_headers: BTreeMap<String, Vec<McpToolHeader>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HttpAuthTokenSource {
    None,
    Config,
    OAuthStore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedHttpAuthToken {
    pub token: Option<String>,
    pub source: HttpAuthTokenSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpToolHeader {
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

    /// Shorten the stdio cumulative response deadline so a test can prove
    /// the end-to-end bound without waiting the full production budget.
    #[cfg(test)]
    pub(crate) async fn set_stdio_response_deadline_for_test(&self, deadline: std::time::Duration) {
        let mut guard = self.inner.lock().await;
        if let Some(McpClientInner::Stdio(inner)) = guard.as_mut() {
            inner.response_deadline = deadline;
        }
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
        let record_request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.clone(),
        });
        let started_at = crate::clock_mock::instant_now();
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        let result = match inner {
            McpClientInner::Stdio(inner) => stdio_call_raw(inner, &self.name, method, params).await,
            McpClientInner::Http(inner) => http_call_raw(inner, &self.name, method, params).await,
        };
        let latency_ms = crate::clock_mock::instant_now()
            .duration_since(started_at)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let record_response = match &result {
            Ok(response) => response.clone(),
            Err(error) => serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "message": error.to_string(),
                }
            }),
        };
        crate::testbench::tape::record_mcp_json_rpc(
            &self.name,
            method,
            &record_request,
            &record_response,
            latency_ms,
        );
        result
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

#[derive(Clone)]
pub(crate) struct HttpStreamConfig {
    client: reqwest::Client,
    url: String,
    auth_token: Option<String>,
    protocol_mode: McpProtocolMode,
    protocol_version: String,
    session_id: Option<String>,
    proxy_server_name: Option<String>,
    server_name: String,
}

pub(crate) struct McpConnectOptions {
    protocol_mode: McpProtocolMode,
    protocol_version: String,
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

#[derive(Clone, Debug)]
pub(crate) struct McpInputRound {
    input_responses: serde_json::Value,
    request_state: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_stdio_deadline;
