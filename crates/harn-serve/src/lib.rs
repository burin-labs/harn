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

pub use adapter::{AdapterDescriptor, TransportAdapter};
pub use adapters::a2a::{A2aHttpServeOptions, A2aServer, A2aServerConfig, A2A_PROTOCOL_VERSION};
pub use adapters::acp::{
    run_acp_channel_server, run_acp_server, AcpRuntimeConfigurator, AcpServer, AcpServerConfig,
    NoopAcpRuntimeConfigurator,
};
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
