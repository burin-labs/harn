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
    /// Call one tool on a stdio MCP server command and print a JSON report.
    Call(McpCallArgs),
    /// Record, replay, verify, or simulate MCP servers for deterministic evals.
    Mock(McpMockArgs),
    /// Log in to a remote MCP server via OAuth.
    Login(McpLoginArgs),
    /// Remove a stored OAuth token.
    Logout(McpServerRefArgs),
    /// Show stored OAuth status for a server.
    Status(McpServerRefArgs),
    /// Discover an unofficial /.well-known/mcp.json MCP endpoint descriptor.
    Discover(McpDiscoverArgs),
    /// Print the default OAuth redirect URI.
    RedirectUri,
    /// List the canonical catalog of well-known MCP server presets.
    Presets(McpPresetsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct McpCallArgs {
    /// MCP tool name to invoke.
    #[arg(long, value_name = "NAME")]
    pub tool: String,
    /// JSON object to pass as the tool arguments.
    #[arg(long, default_value = "{}", value_name = "JSON")]
    pub arguments: String,
    /// Optional MCP progress token; matching progress notifications are counted.
    #[arg(long = "progress-token", value_name = "TOKEN")]
    pub progress_token: Option<String>,
    /// Upstream stdio MCP server command. Pass after `--`.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct McpPresetsArgs {
    /// Emit the catalog as a stable JSON envelope instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct McpDiscoverArgs {
    /// Website or server URL whose origin should publish /.well-known/mcp.json.
    pub url: String,
    /// Emit a stable JSON envelope for app/script integration.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct McpMockArgs {
    #[command(subcommand)]
    pub command: McpMockCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpMockCommand {
    /// Proxy a stdio MCP server and write a redacted JSON-RPC cassette.
    Record(McpMockRecordArgs),
    /// Serve a redacted cassette back as a credential-free stdio MCP server.
    Replay(McpMockReplayArgs),
    /// Diff a cassette against another cassette or a freshly probed stdio server.
    Verify(McpMockVerifyArgs),
    /// Serve a seeded stateful simulated world over stdio MCP.
    World(McpMockWorldArgs),
    /// Score one or more final world states against a world spec.
    Eval(McpMockEvalArgs),
}

#[derive(Debug, Args)]
pub(crate) struct McpMockRecordArgs {
    /// Path to write the redacted cassette JSON.
    #[arg(long, value_name = "PATH")]
    pub cassette: String,
    /// Upstream stdio MCP server command. Pass after `--`.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct McpMockReplayArgs {
    /// Cassette JSON produced by `harn mcp mock record`.
    #[arg(long, value_name = "PATH")]
    pub cassette: String,
}

#[derive(Debug, Args)]
pub(crate) struct McpMockVerifyArgs {
    /// Recorded cassette to treat as the baseline.
    #[arg(long, value_name = "PATH")]
    pub cassette: String,
    /// Candidate cassette to diff against the baseline.
    #[arg(long, value_name = "PATH", conflicts_with = "command")]
    pub candidate: Option<String>,
    /// Write the structured verify report here. Defaults to stdout.
    #[arg(long, value_name = "PATH")]
    pub report: Option<String>,
    /// Candidate stdio MCP server command. Pass after `--`.
    #[arg(last = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct McpMockWorldArgs {
    /// World spec JSON.
    #[arg(long, value_name = "PATH")]
    pub spec: String,
    /// Write the final state JSON when stdin closes.
    #[arg(long = "state-out", value_name = "PATH")]
    pub state_out: Option<String>,
    /// Write a world eval report against `goal_state` when stdin closes.
    #[arg(long, value_name = "PATH")]
    pub report: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct McpMockEvalArgs {
    /// World spec JSON containing `initial_state` and `goal_state`.
    #[arg(long, value_name = "PATH")]
    pub spec: String,
    /// Final state JSON. Repeat to compute pass-rate and pass^k.
    #[arg(long = "state", value_name = "PATH", required = true)]
    pub states: Vec<String>,
    /// Write the structured report here. Defaults to stdout.
    #[arg(long, value_name = "PATH")]
    pub report: Option<String>,
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
    /// Bulk first-auth: log in to every OAuth-backed MCP server in the nearest
    /// harn.toml that isn't already connected.
    #[arg(long, conflicts_with_all = ["target", "url"])]
    pub all: bool,
    /// Bulk re-auth: re-authenticate servers whose stored token is expired or
    /// revoked. Combine with --all to force-reauth every server.
    #[arg(long, conflicts_with_all = ["target", "url"])]
    pub reauth: bool,
    /// With --all/--reauth, limit the bulk set to these server names.
    #[arg(long = "only", value_delimiter = ',', value_name = "NAME")]
    pub only: Vec<String>,
    /// Max concurrent flows to prepare during a bulk login (overrides config).
    #[arg(long)]
    pub concurrency: Option<usize>,
    /// Emit machine-readable JSON status lines (one per event) during a bulk
    /// login instead of the human-readable progress display.
    #[arg(long)]
    pub json: bool,
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
