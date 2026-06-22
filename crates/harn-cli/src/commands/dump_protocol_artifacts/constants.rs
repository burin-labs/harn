use harn_serve::adapters::acp::HARN_PROVIDER_CATALOG_METHOD;
use harn_serve::MCP_PROTOCOL_VERSION;

pub(super) const ACP_AGENT_METHODS: &[&str] = &[
    "initialize",
    "session/inject",
    "session/new",
    "session/load",
    "session/replace_inject",
    "session/resume",
    "session/prompt",
    "session/revoke_inject",
    "session/truncate",
    "session/rollback",
    "session/redo",
    "session/remind",
    "session/pending_injections",
    "session/revoke_reminder",
    "session/cancel_tool_call",
    "session/close",
    "session/stop",
];

pub(super) const ACP_DEPRECATED_AGENT_METHODS: &[DeprecatedWireValue] = &[DeprecatedWireValue {
    value: "session/stop",
    replacement: "session/close",
}];

pub(super) const ACP_CLIENT_METHODS: &[&str] = &[
    "fs/read_text_file",
    "fs/write_text_file",
    "terminal/create",
    "terminal/kill",
    "session/request_permission",
];

/// Every JSON-RPC method the ACP adapter's `dispatch` actually handles,
/// reconciled against the match arms in
/// `crates/harn-serve/src/adapters/acp/dispatch.rs`.
///
/// `ACP_AGENT_METHODS` is the stable, hand-curated host-facing contract that
/// the TypeScript/Swift/Python/Go bindings publish. The dispatcher additionally
/// services workspace-management, workflow-control, and HITL methods that those
/// bindings intentionally do not expose as typed enums yet. The Rust artifact
/// is the first binding to publish the *complete* handled surface so downstream
/// Rust hosts (Burin Code, harn-cloud) can route every method without
/// re-deriving it from the dispatcher by hand. Keep this list in lockstep with
/// the `match method.as_str()` arms; `dispatched_acp_methods_match_artifact`
/// guards against drift.
pub(super) const ACP_DISPATCHED_METHODS: &[&str] = &[
    "initialize",
    "authenticate",
    HARN_PROVIDER_CATALOG_METHOD,
    "session/new",
    "session/load",
    "session/resume",
    "session/fork",
    "session/truncate",
    "session/rollback",
    "session/redo",
    "session/set_mode",
    "session/set_config_option",
    "session/fs_mode",
    "session/fs_commit_staged",
    "session/fs_discard_staged",
    "session/restore_tool_call",
    "session/prompt",
    "session/cancel",
    "session/cancel_tool_call",
    "session/close",
    "session/stop",
    "session/inject",
    "session/revoke_inject",
    "session/replace_inject",
    "session/remind",
    "session/pending_injections",
    "session/revoke_reminder",
    "session/list",
    "harn.session_workspace_roots",
    "harn.session_add_root",
    "harn.session_reanchor",
    "harn.session_rollback",
    "harn.session_redo",
    "agent/resume",
    "harn.hitl.respond",
    "workflow/signal",
    "harn.workflow.signal",
    "workflow/query",
    "harn.workflow.query",
    "workflow/update",
    "harn.workflow.update",
    "workflow/pause",
    "harn.workflow.pause",
    "workflow/resume",
    "harn.workflow.resume",
    "mcp/catalog",
    "harn.mcp.catalog",
    "mcp/status",
    "harn.mcp.status",
    "mcp/authorize",
    "harn.mcp.authorize",
    "mcp/authorize_batch",
    "harn.mcp.authorize_batch",
    "mcp/oauth_callback",
    "harn.mcp.oauth_callback",
    "mcp/import_token",
    "harn.mcp.import_token",
];

pub(super) const ACP_AGENT_NOTIFICATIONS: &[&str] =
    &["session/message", "session/update", "terminal/output"];

pub(super) const ACP_CONTENT_BLOCK_TYPES: &[&str] =
    &["text", "resource_link", "resource", "image", "audio"];

pub(super) const ACP_TOOL_EXECUTOR_SIMPLE_VALUES: &[&str] =
    &["harn_builtin", "host_bridge", "provider_native"];

pub(super) const A2A_METHODS: &[&str] = &[
    "message/send",
    "message/stream",
    "tasks/get",
    "tasks/cancel",
    "tasks/resubscribe",
    "tasks/pushNotificationConfig/set",
    "tasks/pushNotificationConfig/get",
    "tasks/pushNotificationConfig/list",
    "tasks/pushNotificationConfig/delete",
    "agent/getAuthenticatedExtendedCard",
];

pub(super) const A2A_TASK_STATES: &[&str] = &[
    "submitted",
    "working",
    "completed",
    "failed",
    "canceled",
    "cancelled",
    "input-required",
    "rejected",
    "auth-required",
];

pub(super) const A2A_TASK_EVENT_TYPES: &[&str] = &["status", "message", "worker_update"];

pub(super) const MCP_DRAFT_PROTOCOL_VERSION: &str = "DRAFT-2026-v1";

pub(super) const MCP_FINAL_2026_PROTOCOL_VERSION: &str = "2026-07-28";

pub(super) const MCP_JSON_SCHEMA_2020_12_DIALECT: &str =
    "https://json-schema.org/draft/2020-12/schema";

pub(super) const MCP_PROTOCOL_VERSIONS: &[&str] =
    &[MCP_DRAFT_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION];

pub(super) const MCP_METHODS: &[&str] = &[
    "server/discover",
    "initialize",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/read",
    "resources/templates/list",
    "prompts/list",
    "prompts/get",
    "completion/complete",
    "logging/setLevel",
    "sampling/createMessage",
    "elicitation/create",
    "notifications/initialized",
    "notifications/message",
];

pub(super) const MCP_REQUIRED_METADATA_KEYS: &[&str] = &[
    "io.modelcontextprotocol/protocolVersion",
    "io.modelcontextprotocol/clientInfo",
    "io.modelcontextprotocol/clientCapabilities",
];

pub(super) const MCP_METADATA_KEYS: &[&str] = &[
    "io.modelcontextprotocol/protocolVersion",
    "io.modelcontextprotocol/clientInfo",
    "io.modelcontextprotocol/clientCapabilities",
    "io.modelcontextprotocol/logLevel",
    "progressToken",
    "traceparent",
    "tracestate",
    "baggage",
];

pub(super) const MCP_STANDARD_HTTP_HEADERS: &[&str] =
    &["MCP-Protocol-Version", "Mcp-Method", "Mcp-Name"];

pub(super) const MCP_CACHE_RESULT_FIELDS: &[&str] = &["ttlMs", "cacheScope"];

pub(super) const MCP_CACHE_SCOPES: &[&str] = &["private", "public"];

pub(super) const MCP_RESULT_TYPES: &[&str] = &["complete", "input_required"];

pub(super) const MCP_INPUT_REQUIRED_RESULT_TYPE: &str = "input_required";

pub(super) const MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE: i32 = -32004;

pub(super) const MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE: &str =
    "Unsupported protocol version";

pub(super) const MCP_OAUTH_CLIENT_REGISTRATION_MODES: &[&str] = &[
    "pre_registered",
    "client_id_metadata_document",
    "dynamic_client_registration",
    "manual",
];

pub(super) const MCP_OAUTH_AUTH_MODES: &[&str] = &["cimd", "dcr", "static", "byo"];

pub(super) const MCP_OAUTH_APPLICATION_TYPES: &[&str] = &["native", "web"];

pub(super) const MCP_LOGGING_LEVELS: &[&str] = &[
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

pub(super) const SCHEMA_COPIES: &[SchemaCopy] = &[
    SchemaCopy {
        protocol: "acp",
        source: "conformance/protocols/schemas/acp-session-update.schema.json",
        artifact: "schemas/acp-session-update.schema.json",
    },
    SchemaCopy {
        protocol: "a2a",
        source: "conformance/protocols/schemas/a2a-0.3.0.schema.json",
        artifact: "schemas/a2a-0.3.0.schema.json",
    },
    SchemaCopy {
        protocol: "mcp",
        source: "conformance/protocols/schemas/mcp-2025-11-25.schema.json",
        artifact: "schemas/mcp-2025-11-25.schema.json",
    },
    SchemaCopy {
        protocol: "mcp",
        source: "conformance/protocols/schemas/mcp-draft-2026-v1.schema.json",
        artifact: "schemas/mcp-draft-2026-v1.schema.json",
    },
];

pub(super) struct SchemaCopy {
    pub(super) protocol: &'static str,
    pub(super) source: &'static str,
    pub(super) artifact: &'static str,
}

pub(super) struct DeprecatedWireValue {
    pub(super) value: &'static str,
    pub(super) replacement: &'static str,
}
