#![recursion_limit = "256"]

mod adapter;
pub mod adapters;
mod auth;
mod core;
mod error;
mod exports;
pub mod http_codec;
pub mod limits;
mod mcp_context;
pub mod mcp_host_bridge;
mod mcp_prompts;
pub mod permissions;
#[cfg(test)]
mod protocol_fixture_tests;
mod replay;
pub mod sessions;
pub mod tls;
pub mod transport;
pub mod ws;

/// Default 10 MiB body size cap applied to every HTTP router exposed by
/// the `harn-serve` adapters (MCP, A2A, API). Matches the orchestrator
/// listener's `DEFAULT_MAX_BODY_BYTES` so large/malicious POSTs cannot
/// exhaust process memory while axum buffers the request before
/// handing it to a handler.
pub const DEFAULT_HTTP_BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

pub use adapter::{AdapterDescriptor, TransportAdapter};
pub use adapters::a2a::{A2aHttpServeOptions, A2aServer, A2aServerConfig, A2A_PROTOCOL_VERSION};
pub use adapters::acp::{
    run_acp_channel_server, run_acp_server, AcpProfileConfig, AcpRuntimeConfigurator, AcpServer,
    AcpServerConfig, NoopAcpRuntimeConfigurator,
};
pub use adapters::api::{ApiHttpServeOptions, ApiServer, ApiServerConfig};
pub use adapters::mcp::{
    McpHttpServeOptions, McpServer, McpServerConfig, McpStdioServer, MCP_PROTOCOL_VERSION,
};
pub use auth::{
    AllowlistOutcome, ApiKeyAuthConfig, ApiKeyEntry, AuthMethodConfig, AuthPolicy, AuthRequest,
    AuthenticatedPrincipal, AuthorizationDecision, HmacAuthConfig, McpAllowlist, McpAllowlistTools,
    OAuth21AuthConfig, OAuthClaims,
};
pub use core::{
    CallArguments, CallRequest, CallResponse, DispatchCore, DispatchCoreConfig, NoopVmConfigurator,
    VmConfigurator,
};
pub use error::{forbidden_data_payload, forbidden_message, DispatchError};
pub use exports::{ExportCatalog, ExportedCallableKind, ExportedFunction, ExportedParam};
pub use http_codec::{
    axum_response_from_call, axum_response_from_dispatch_error, classify_ws_upgrade,
    decode_call_response, dispatch_error_payload, fresh_request_id, HttpCodecOutcome, SseEventSpec,
};
pub use limits::{
    Algorithm, BudgetSpec, InMemoryLimitStore, LimitContext, LimitDecision, LimitGuard,
    LimitRegistry, LimitScope, LimitStats, LimitStore, Quota, QuotaWindow, RouteLimits,
    TenantOverride,
};
pub use mcp_host_bridge::install_mcp_host_allowlist;
pub use mcp_prompts::FilePromptCatalog;
pub use permissions::{
    ActionClass, AuditEntry, AuditFilter, AuditOutcome, DecisionScope, InMemoryConfig,
    InMemoryPermissionStore, LlmPolicy, PermissionDecision, PermissionPolicy, PermissionRequest,
    PermissionStore, PolicyVersion, RedactionPolicy, RememberRule, RememberSpec, Risk, RuleId,
};
pub use replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
pub use sessions::{
    sessions_router, AppendEvent, ArchiveSink, CreateSession, EventId, EventPage, ForkResult,
    ListFilter, MemorySessionStore, ReadRange, RetentionPolicy, SessionEventKind, SessionId,
    SessionMeta, SessionSigner, SessionStatus, SessionStore, SharedArchiveSink, SharedSessionStore,
    Snapshot, SnapshotId, SqliteSessionStore, StoreError, StoreHooks, StoreResult, StoredEvent,
    SweepReport, Tombstone, TruncateResult, VerifyFailure, VerifyReport,
};
pub use tls::{bind_listener, HstsConfig, HttpTlsConfig};
pub use transport::{
    apply_transport_layers, compute_strong_etag, CorsConfig, HeaderOptOutPredicate,
    TransportConfig, COMPRESSION_MIN_SIZE_BYTES, COMPRESSION_OPT_OUT_HEADER,
    COMPRESSION_OPT_OUT_VALUE,
};
pub use ws::{
    ws_route, WsConfig, WsError, WsMessage, WsSession, DEFAULT_IDLE_PING_MS,
    DEFAULT_MAX_MESSAGE_BYTES,
};
