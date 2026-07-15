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

/// Whether a terminal tool result changed workspace state. This is
/// intentionally orthogonal to [`ToolCallStatus`]: a successfully completed
/// tool can be a no-op, while a failed tool may have applied a mutation before
/// reporting a post-apply error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolMutationStatus {
    /// The tool changed workspace state.
    Applied,
    /// The tool did not change workspace state.
    NotApplied,
    /// The execution boundary did not provide a definitive outcome.
    Unknown,
}

impl ToolMutationStatus {
    pub const ALL: [Self; 3] = [Self::Applied, Self::NotApplied, Self::Unknown];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::NotApplied => "not_applied",
            Self::Unknown => "unknown",
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
    /// The host bridge returned an error during dispatch.
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
    /// The agent loop reached a terminal condition (completion judge `done`,
    /// max iterations, budget exhausted, stuck) while this call was still in
    /// flight — a `ToolCall` start was observed but the call never dispatched
    /// to a `Completed`/`Failed` result. The loop synthesizes a terminal
    /// update in this category at session finalize so the transcript never
    /// ends with a dangling `pending` call. Distinct from [`Self::Cancelled`]
    /// (an explicit `cancel_in_flight_tool_call` / user preemption) so an
    /// auditor can tell loop-lifecycle abandonment from a user-initiated stop.
    AbandonedAtLoopExit,
    /// Default when classification was not performed.
    Unknown,
}

impl ToolCallErrorCategory {
    pub const ALL: [Self; 12] = [
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
        Self::AbandonedAtLoopExit,
        Self::Unknown,
    ];

    /// Whether a rejection in this category is RECOVERABLE by the model on its
    /// own — i.e. the call failed because of a fixable slip (bad/missing
    /// arguments, malformed tool name) and re-issuing it *with the correction*
    /// is the right next move. Distinguished from a true policy/permission
    /// denial, where the model must NOT retry and should pivot or ask. Used by
    /// the dispatch primitive to pick a retry-positive vs. don't-retry feedback
    /// body for the model-facing tool result.
    pub fn is_recoverable(self) -> bool {
        matches!(self, Self::SchemaValidation)
    }

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
            Self::AbandonedAtLoopExit => "abandoned_at_loop_exit",
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
            | Internal::ChannelClosed
            | Internal::NotFound
            | Internal::CircuitOpen
            | Internal::BudgetExceeded
            // An internal engine/wiring bug is a host-side failure, not the
            // tool's fault; it normally propagates out of the loop, but if one
            // is ever recorded as a tool event, `HostBridgeError` is the honest
            // wire bucket.
            | Internal::Internal
            | Internal::Generic => Self::HostBridgeError,
        }
    }
}

/// Which gate refused a tool call. Pairs with [`ToolDenial`] so host
/// harnesses can distinguish a hard capability/policy ceiling (terminal —
/// retrying the identical call can never succeed) from a user/host
/// approval rejection, without re-parsing the human-readable reason
/// string (harn#2780).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialGate {
    /// The tool is not in the policy's allowed-tool list.
    ToolCeiling,
    /// The model emitted Harn text-tool wrapper syntax as a native/provider
    /// tool name or arguments payload, so dispatch refused the wrapper while
    /// coaching a direct re-issue of the embedded tool call.
    MalformedToolWrapper,
    /// The tool requires a capability/operation the policy does not grant
    /// (e.g. `workspace.write_text`, `process.exec`).
    CapabilityCeiling,
    /// The tool's side-effect level exceeds the policy ceiling
    /// (e.g. a `process_exec` tool under a `read_only` policy).
    SideEffectCeiling,
    /// A `tool_arg_constraint` allow-list rejected the resolved argument
    /// value (e.g. a `command` that does not match `cargo *`).
    ArgConstraint,
    /// A dynamic permission rule (`when`/`unless` predicate) denied the
    /// call.
    DynamicPermission,
    /// A static approval policy decided `deny`.
    ApprovalPolicy,
    /// Approval was required (`ask`) but could not be requested because no
    /// host bridge was available or the request transport failed.
    ApprovalUnavailable,
    /// The host/user rejected an approval request (`session/request_permission`).
    HostRejected,
    /// A registered pre-tool hook returned `deny`.
    HookDeny,
    /// Gate could not be classified.
    #[default]
    Unknown,
}

impl DenialGate {
    pub const ALL: [Self; 11] = [
        Self::ToolCeiling,
        Self::MalformedToolWrapper,
        Self::CapabilityCeiling,
        Self::SideEffectCeiling,
        Self::ArgConstraint,
        Self::DynamicPermission,
        Self::ApprovalPolicy,
        Self::ApprovalUnavailable,
        Self::HostRejected,
        Self::HookDeny,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolCeiling => "tool_ceiling",
            Self::MalformedToolWrapper => "malformed_tool_wrapper",
            Self::CapabilityCeiling => "capability_ceiling",
            Self::SideEffectCeiling => "side_effect_ceiling",
            Self::ArgConstraint => "arg_constraint",
            Self::DynamicPermission => "dynamic_permission",
            Self::ApprovalPolicy => "approval_policy",
            Self::ApprovalUnavailable => "approval_unavailable",
            Self::HostRejected => "host_rejected",
            Self::HookDeny => "hook_deny",
            Self::Unknown => "unknown",
        }
    }
}

/// Structured record of a tool call refused at the dispatch boundary —
/// by a capability/policy ceiling, an argument allow-list, a permission
/// rule, an approval decision, or a pre-tool hook. Carried on the denied
/// `tool_result` and the `PermissionDeny` transcript event so host
/// harnesses (and the loop's own stall detector) can fail or pivot early
/// without re-parsing human-readable command output (harn#2780). The
/// `denied_paths` field captures any workspace paths the refused call
/// declared, so a path-scoped denial names the offending path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDenial {
    /// Which gate refused the call.
    pub gate: DenialGate,
    /// Capability/operation that was exceeded, e.g. `workspace.read_text`
    /// or `process.exec`, when the gate identified one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub capability: Option<String>,
    /// Workspace paths the denied call declared, when the tool annotates
    /// path arguments. Empty for tools that declare no paths.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub denied_paths: Vec<String>,
    /// Whether re-issuing the identical call could ever succeed. Capability
    /// and side-effect ceilings, argument allow-lists, and policy/approval
    /// denials are terminal (`false`); a host harness should fail or pivot
    /// rather than spend another model call retrying.
    pub retryable: bool,
    /// Human-readable explanation — the same text the model sees in the
    /// tool result.
    pub reason: String,
}

impl ToolDenial {
    /// Build a terminal denial (`retryable: false`) with no declared paths
    /// attached yet. Every gate Harn currently enforces is terminal —
    /// re-issuing the identical call can never succeed — so the constructor
    /// hard-codes `retryable: false`; the field exists so a future soft
    /// denial can set it `true`. Callers at the dispatch boundary enrich
    /// `denied_paths` from the tool's annotated path arguments.
    pub fn terminal(
        gate: DenialGate,
        capability: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            gate,
            capability,
            denied_paths: Vec::new(),
            retryable: false,
            reason: reason.into(),
        }
    }

    /// Build a SOFT denial (`retryable: true`): the call was refused for *this*
    /// argument, but re-issuing it with a corrected argument can succeed — so
    /// the model should be coached to retry with the correction rather than told
    /// to give up. Used for the argument allow-list gate (`ArgConstraint`),
    /// where a path/command outside the allowed scope is a fixable slip, not a
    /// hard capability ceiling. The dispatch boundary routes a retryable denial
    /// through the recoverable (retry-positive) tool-result body.
    pub fn retryable(
        gate: DenialGate,
        capability: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            gate,
            capability,
            denied_paths: Vec::new(),
            retryable: true,
            reason: reason.into(),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
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
    /// (host IDE bridge and CLI host shells).
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
