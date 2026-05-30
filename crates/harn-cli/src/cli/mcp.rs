use std::net::SocketAddr;

use clap::{Args, Subcommand};

use super::orchestrator::OrchestratorLocalArgs;
use super::serve::McpServeTransport;

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Expose a running orchestrator as an MCP server.
    Serve(McpServeArgs),
    /// Log in to a remote MCP server via OAuth.
    Login(McpLoginArgs),
    /// Remove a stored OAuth token.
    Logout(McpServerRefArgs),
    /// Show stored OAuth status for a server.
    Status(McpServerRefArgs),
    /// Print the default OAuth redirect URI.
    RedirectUri,
}

#[derive(Debug, Args)]
pub(crate) struct McpServeArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Transport to expose for MCP clients.
    #[arg(long, value_enum, default_value_t = McpServeTransport::Stdio)]
    pub transport: McpServeTransport,
    /// Socket address to bind when serving over HTTP.
    #[arg(
        long,
        env = "HARN_MCP_SERVE_BIND",
        default_value = "127.0.0.1:8765",
        value_name = "ADDR"
    )]
    pub bind: SocketAddr,
    /// Streamable HTTP endpoint path.
    #[arg(long, default_value = "/mcp", value_name = "PATH")]
    pub path: String,
    /// Legacy SSE endpoint path for older MCP clients.
    #[arg(long = "sse-path", default_value = "/sse", value_name = "PATH")]
    pub sse_path: String,
    /// Legacy SSE POST endpoint path for older MCP clients.
    #[arg(
        long = "messages-path",
        default_value = "/messages",
        value_name = "PATH"
    )]
    pub messages_path: String,
}

#[derive(Debug, Args)]
pub(crate) struct McpLoginArgs {
    /// MCP server name from harn.toml or a direct URL.
    pub target: Option<String>,
    /// Explicit server URL for ad hoc login or status checks.
    #[arg(long)]
    pub url: Option<String>,
    /// Explicit OAuth client ID.
    #[arg(long = "client-id")]
    pub client_id: Option<String>,
    /// Explicit OAuth client secret.
    #[arg(long = "client-secret")]
    pub client_secret: Option<String>,
    /// Requested OAuth scope string.
    #[arg(long = "scope")]
    pub scope: Option<String>,
    /// OAuth redirect URI for the local callback listener.
    #[arg(
        long = "redirect-uri",
        default_value = "http://127.0.0.1:9783/oauth/callback"
    )]
    pub redirect_uri: String,
}

#[derive(Debug, Args)]
pub(crate) struct McpServerRefArgs {
    /// MCP server name from harn.toml or a direct URL.
    pub target: Option<String>,
    /// Explicit server URL for ad hoc login or status checks.
    #[arg(long)]
    pub url: Option<String>,
    /// Emit machine-readable JSON instead of a human summary. With no
    /// target, `mcp status` reports every configured MCP server; with a
    /// target it reports that one server's OAuth status.
    #[arg(long)]
    pub json: bool,
}
