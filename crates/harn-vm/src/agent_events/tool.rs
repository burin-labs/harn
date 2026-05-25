use serde::{Deserialize, Serialize};

/// Status of a tool call. Mirrors ACP's `toolCallStatus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Dispatched by the model but not yet started.
    Pending,
    /// Dispatch is actively running.
    InProgress,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
}

impl ToolCallStatus {
    pub const ALL: [Self; 4] = [
        Self::Pending,
        Self::InProgress,
        Self::Completed,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Wire-level classification of a `ToolCallUpdate` failure. Pairs with the
/// human-readable `error` string so clients can render each failure type
/// distinctly (e.g. surface a "permission denied" badge, or a different
/// retry affordance for `network` vs `tool_error`). The enum is
/// deliberately extensible — `unknown` is the default when the runtime
/// could not classify a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallErrorCategory {
    /// Host-side validation rejected the args (missing required field,
    /// invalid type, malformed JSON).
    SchemaValidation,
    /// The tool ran and returned an error result (e.g. `read_file` on a
    /// missing path) — distinguished from a transport failure.
    ToolError,
    /// MCP transport / server-protocol error.
    McpServerError,
    /// Burin Swift host bridge returned an error during dispatch.
    HostBridgeError,
    /// `session/request_permission` denied by the client, or a policy
    /// rule (static or dynamic) refused the call.
    PermissionDenied,
    /// The harn loop detector skipped this call because the same
    /// (tool, args) pair repeated past the configured threshold.
    RejectedLoop,
    /// Streaming text candidate was detected (bare `name(` or
    /// `<tool_call>` opener) but never resolved into a parseable call:
    /// args parsed as malformed, the heredoc body broke, the tag closed
    /// without a balanced expression, or the stream ended mid-call.
    /// Used by the streaming candidate detector (harn#692) to retract a
    /// `tool_call` candidate that turned out to be prose or syntactically
    /// broken so clients can dismiss the in-flight chip.
    ParseAborted,
    /// The tool exceeded its time budget.
    Timeout,
    /// Transient network / rate-limited / 5xx provider failure.
    Network,
    /// The tool was cancelled (e.g. session aborted).
    Cancelled,
    /// Default when classification was not performed.
    Unknown,
}

impl ToolCallErrorCategory {
    pub const ALL: [Self; 11] = [
        Self::SchemaValidation,
        Self::ToolError,
        Self::McpServerError,
        Self::HostBridgeError,
        Self::PermissionDenied,
        Self::RejectedLoop,
        Self::ParseAborted,
        Self::Timeout,
        Self::Network,
        Self::Cancelled,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SchemaValidation => "schema_validation",
            Self::ToolError => "tool_error",
            Self::McpServerError => "mcp_server_error",
            Self::HostBridgeError => "host_bridge_error",
            Self::PermissionDenied => "permission_denied",
            Self::RejectedLoop => "rejected_loop",
            Self::ParseAborted => "parse_aborted",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// Map an internal `ErrorCategory` (used by the VM's `VmError`
    /// classification) onto the wire enum. The internal taxonomy is
    /// finer-grained — several transient categories collapse onto
    /// `Network`, and the auth/quota family becomes `HostBridgeError`
    /// because at the tool-dispatch boundary those errors come from
    /// the bridge transport rather than the tool itself.
    pub fn from_internal(category: &crate::value::ErrorCategory) -> Self {
        use crate::value::ErrorCategory as Internal;
        match category {
            Internal::Timeout => Self::Timeout,
            Internal::RateLimit
            | Internal::Overloaded
            | Internal::ServerError
            | Internal::TransientNetwork => Self::Network,
            Internal::SchemaValidation | Internal::SchemaStreamAborted => Self::SchemaValidation,
            Internal::ToolError => Self::ToolError,
            Internal::ToolRejected => Self::PermissionDenied,
            Internal::Cancelled => Self::Cancelled,
            Internal::Auth
            | Internal::EgressBlocked
            | Internal::NotFound
            | Internal::CircuitOpen
            | Internal::BudgetExceeded
            | Internal::Generic => Self::HostBridgeError,
        }
    }
}

/// Where a tool actually ran. Tags `ToolCallUpdate` so clients can render
/// "via mcp:linear" / "via host bridge" badges, attribute latency by
/// transport, and route errors to the right surface (harn#691).
///
/// On the wire this serializes adjacently-tagged so the `mcp_server`
/// case carries the configured server name. The ACP adapter rewrites
/// unit variants as bare strings (`"harn_builtin"`, `"host_bridge"`,
/// `"provider_native"`) and the `McpServer` case as
/// `{"kind": "mcp_server", "serverName": "..."}` to match the protocol's
/// camelCase convention.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolExecutor {
    /// VM-stdlib (`read_file`, `write_file`, `exec`, `http_*`, `mcp_*`)
    /// or any Harn-side handler closure registered in `tools_val`.
    HarnBuiltin,
    /// Capability provided by the host through `HostBridge.builtin_call`
    /// (Swift-side IDE bridge, BurinApp, BurinCLI host shells).
    HostBridge,
    /// Tool dispatched against a configured MCP server. Detected by the
    /// `_mcp_server` tag that `mcp_list_tools` injects on every tool
    /// dict before the agent loop sees it.
    McpServer { server_name: String },
    /// Provider-side server-side tool execution — currently OpenAI
    /// Responses-API server tools (e.g. native `tool_search`). The
    /// runtime never dispatches these locally; the model returns the
    /// already-executed result inline.
    ProviderNative,
}
