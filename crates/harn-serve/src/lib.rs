mod adapter;
pub mod adapters;
mod auth;
mod core;
mod error;
mod exports;
mod replay;

pub use adapter::{AdapterDescriptor, TransportAdapter};
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
pub use exports::{ExportCatalog, ExportedFunction, ExportedParam};
pub use replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
