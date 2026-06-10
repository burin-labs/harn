#![recursion_limit = "256"]

mod adapter;
pub mod adapters;
mod auth;
mod auth_context;
mod core;
pub mod embed;
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
pub mod streaming;
pub mod tls;
pub mod transport;
pub mod ws;

/// Default 10 MiB body size cap applied to every HTTP router exposed by
/// the `harn-serve` adapters (MCP, A2A, API). Matches the orchestrator
/// listener's `DEFAULT_MAX_BODY_BYTES` so large/malicious POSTs cannot
/// exhaust process memory while axum buffers the request before
/// handing it to a handler.
pub const DEFAULT_HTTP_BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

pub(crate) use adapter::DispatchRuntime;
pub use adapter::{AdapterDescriptor, TransportAdapter};
pub use adapters::a2a::{A2aHttpServeOptions, A2aServer, A2aServerConfig, A2A_PROTOCOL_VERSION};
pub use adapters::acp::{
    run_acp_channel_server, run_acp_channel_server_with_handle, run_acp_server,
    run_acp_websocket_server, AcpChannelHandle, AcpContentBlock, AcpEmbeddedResource, AcpHarnMeta,
    AcpJsonRpcError, AcpJsonRpcErrorResponse, AcpJsonRpcId, AcpJsonRpcRequest, AcpJsonRpcResponse,
    AcpMeta, AcpOutput, AcpProfileConfig, AcpRuntimeConfigurator, AcpSandboxConfig, AcpServer,
    AcpServerConfig, AcpSessionCancelToolCallParams, AcpSessionIdParams, AcpSessionInjectContent,
    AcpSessionInjectMode, AcpSessionInjectParams, AcpSessionMessageIdParams, AcpSessionNewParams,
    AcpSessionPromptParams, AcpSessionPromptResult, AcpSessionReplaceInjectParams,
    AcpSessionRestoreResult, AcpWebSocketServeOptions, NoopAcpRuntimeConfigurator,
    ACP_METHOD_INITIALIZE, ACP_METHOD_SESSION_CANCEL, ACP_METHOD_SESSION_CANCEL_TOOL_CALL,
    ACP_METHOD_SESSION_CLOSE, ACP_METHOD_SESSION_INJECT, ACP_METHOD_SESSION_NEW,
    ACP_METHOD_SESSION_PENDING_INJECTIONS, ACP_METHOD_SESSION_PROMPT,
    ACP_METHOD_SESSION_REPLACE_INJECT, ACP_METHOD_SESSION_REVOKE_INJECT,
};
pub use adapters::api::{ApiHttpServeOptions, ApiServer, ApiServerConfig};
pub use adapters::mcp::{
    McpHttpServeOptions, McpServer, McpServerConfig, McpStdioServer, MCP_PROTOCOL_VERSION,
};
pub use adapters::site::{
    SiteAuth, SiteAuthContext, SiteAuthOutcome, SiteHttpServeOptions, SiteServer, SiteServerConfig,
    SiteStreamProvider,
};
pub use adapters::worker::{
    run_job_from_files, run_job_once, run_job_once_with, start_worker_server, JobRunOutcome,
    WorkerJobRegistration, WorkerServeOptions, WorkerServer, WorkerShutdownReport,
};
pub use auth::{
    AllowlistOutcome, ApiKeyAuthConfig, ApiKeyEntry, AuthMethodConfig, AuthPolicy, AuthRequest,
    AuthenticatedPrincipal, AuthorizationDecision, HmacAuthConfig, McpAllowlist, McpAllowlistTools,
    OAuth21AuthConfig, OAuthClaims, ACP_LOCAL_NONE_METHOD_ID,
};
pub use auth_context::{current_auth_context, enter_auth_context, AuthContextScopeGuard};
pub use core::{
    CallArguments, CallRequest, CallResponse, DispatchCore, DispatchCoreConfig, NoopVmConfigurator,
    VmConfigurator,
};
pub use embed::EmbeddedAgent;
pub use error::{forbidden_data_payload, forbidden_message, DispatchError};
pub use exports::{
    emit_export_diagnostics, ExportCatalog, ExportDiagnostic, ExportedCallableKind,
    ExportedFunction, ExportedParam, JobSpec, RetryBackoff, RetrySpec, RouteSpec, ScheduleSpec,
};
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
pub use replay::{InMemoryReplayCache, NoReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
pub use sessions::{
    sessions_router, AppendEvent, ArchiveSink, CreateSession, EventId, EventPage, ForkResult,
    ListFilter, MemorySessionStore, ReadRange, RetentionPolicy, SessionEventKind, SessionId,
    SessionMeta, SessionSigner, SessionStatus, SessionStore, SharedArchiveSink, SharedSessionStore,
    Snapshot, SnapshotId, SqliteSessionStore, StoreError, StoreHooks, StoreResult, StoredEvent,
    SweepReport, Tombstone, TruncateResult, VerifyFailure, VerifyReport,
};
pub use streaming::{
    BodyChannelConfig, MultipartField, MultipartStream, MultipartStreamConfig, RequestBodyChannel,
    StreamError, DEFAULT_BODY_CHANNEL_CAPACITY, DEFAULT_FIELD_BYTES_CAPACITY,
    DEFAULT_MAX_FIELD_BYTES, DEFAULT_MULTIPART_OUTER_CAPACITY,
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

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) struct LlmOverrideReset;

    impl Drop for LlmOverrideReset {
        fn drop(&mut self) {
            harn_vm::llm_config::clear_user_overrides();
            harn_vm::llm::capabilities::clear_user_overrides();
        }
    }

    pub(crate) fn fixture_provider_overlay() -> harn_vm::llm_config::ProvidersConfig {
        harn_vm::llm_config::parse_config_toml(
            r#"
[providers.fixture_runtime]
display_name = "Fixture Runtime"
base_url = "https://fixture.example/v1"
base_url_env = "FIXTURE_RUNTIME_BASE_URL"
chat_endpoint = "/chat/completions"
auth_style = "bearer"
auth_env = "FIXTURE_RUNTIME_API_KEY"
features = ["chat", "tools"]
rpm = 42

[aliases]
fixture-default = { id = "fixture-model-v1", provider = "fixture_runtime", tool_format = "native" }

[models.fixture-model-v1]
name = "Fixture Model v1"
provider = "fixture_runtime"
context_window = 12345
runtime_context_window = 8192
capabilities = ["tool_use", "json"]
pricing = { input_per_mtok = 1.25, output_per_mtok = 2.5 }
availability = "serverless"
tier = "small"
family = "fixture-family"
lineage = "fixture-lineage"
"#,
        )
        .expect("fixture provider overlay parses")
    }

    pub(crate) fn fixture_capability_overlay() -> harn_vm::llm::capabilities::CapabilitiesFile {
        toml::from_str(
            r#"
[[provider.fixture_runtime]]
model_match = "fixture-model-v1"
native_tools = true
tool_search = ["hosted"]
vision = true
structured_output = "native"
prompt_caching = true
thinking_modes = ["effort"]
"#,
        )
        .expect("fixture capability overlay parses")
    }
}
