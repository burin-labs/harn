mod adapter;
pub mod adapters;
mod auth;
mod core;
mod error;
mod exports;
mod replay;

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
pub use replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
