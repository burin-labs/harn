#![recursion_limit = "256"]

mod adapter;
pub mod adapters;
mod auth;
mod core;
mod error;
mod exports;
mod mcp_context;
mod mcp_prompts;
#[cfg(test)]
mod protocol_fixture_tests;
mod replay;
pub mod tls;

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
    ApiKeyAuthConfig, AuthMethodConfig, AuthPolicy, AuthRequest, AuthenticatedPrincipal,
    AuthorizationDecision, HmacAuthConfig, OAuth21AuthConfig, OAuthClaims,
};
pub use core::{
    CallArguments, CallRequest, CallResponse, DispatchCore, DispatchCoreConfig, NoopVmConfigurator,
    VmConfigurator,
};
pub use error::DispatchError;
pub use exports::{ExportCatalog, ExportedCallableKind, ExportedFunction, ExportedParam};
pub use mcp_prompts::FilePromptCatalog;
pub use replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
pub use tls::{HstsConfig, HttpTlsConfig};
