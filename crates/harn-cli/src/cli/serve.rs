use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use ipnet::IpNet;

use super::ProfileArgs;

/// Parse a `--trusted-proxy` value: either a CIDR range (`10.0.0.0/8`) or
/// a bare address, which is treated as a single host (`/32` or `/128`).
pub(crate) fn parse_trusted_proxy(value: &str) -> Result<IpNet, String> {
    if let Ok(net) = value.parse::<IpNet>() {
        return Ok(net);
    }
    value
        .parse::<IpAddr>()
        .map(IpNet::from)
        .map_err(|_| format!("`{value}` is not a valid IP address or CIDR range"))
}

/// Observability dev-routing chosen at server startup. Maps to a single
/// concrete backend installed via
/// [`harn_vm::install_obs_default_backend`] before any handler runs;
/// `.harn` code can still override this with `obs.configure(...)` at
/// runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[clap(rename_all = "snake_case")]
pub(crate) enum ServeObsMode {
    /// Resolve from environment variables (`OTEL_EXPORTER_OTLP_ENDPOINT`,
    /// `HONEYCOMB_API_KEY`, etc.) the same way `obs.auto_backend()` does.
    #[default]
    Auto,
    /// Pretty-print spans/metrics/logs to stdout. Convenient for local
    /// development where you want the server output in the same stream
    /// as the rest of the .harn handler's logs.
    Stdout,
    /// Pretty-print to stderr — keeps stdout clean for response bodies
    /// or piping handler output into another tool.
    Stderr,
    /// Force the OTel exporter even if no env-var is set. Falls back to
    /// the OpenTelemetry SDK default endpoint (`localhost:4318`).
    Otel,
    /// Disable observability emission entirely — primitive events still
    /// touch the in-process buffer (so `obs.events()` works for tests)
    /// but nothing leaves the process.
    Off,
}

#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    #[command(subcommand)]
    pub command: ServeCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServeCommand {
    /// Serve a .harn agent over stdio using ACP.
    Acp(ServeAcpArgs),
    /// Serve a .harn agent over HTTP using A2A.
    A2a(A2aServeArgs),
    /// Serve a `.harn` agent over the local Harn Agents HTTP API.
    Api(ApiServeArgs),
    /// Serve a `.harn` file as an MCP server. Exposes either exported
    /// `pub fn` entrypoints (recommended) or, when the script registers
    /// tools/resources/prompts via `mcp_tools(...)` / `mcp_resource(...)`
    /// / `mcp_prompt(...)`, that script-driven surface.
    Mcp(ServeMcpArgs),
    /// Host a `.harn` file's HTTP handlers on a live web server. Every
    /// exported `pub fn` with a `@route("METHOD", "/path")` attribute — or
    /// matching the `handler_*` naming convention — answers that path; the
    /// handler receives a `req` dict and returns a value or an `http_*`
    /// response envelope.
    Site(SiteServeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ServeAcpArgs {
    /// Static API keys accepted by the ACP authenticate method.
    #[arg(long = "api-key", env = "HARN_SERVE_API_KEY", value_delimiter = ',')]
    pub api_key: Vec<String>,
    /// Shared secret for HMAC authentication through the ACP authenticate method.
    #[arg(long = "hmac-secret", env = "HARN_SERVE_HMAC_SECRET")]
    pub hmac_secret: Option<String>,
    /// Enable LLM trace summaries on shutdown.
    #[arg(
        long = "trace",
        env = "HARN_TRACE",
        action = clap::ArgAction::SetTrue,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub trace: bool,
    /// Where to route `harness.obs.*` spans/metrics/logs.
    #[arg(long = "obs", value_enum, default_value_t = ServeObsMode::Auto)]
    pub obs: ServeObsMode,
    #[command(flatten)]
    pub profile: ProfileArgs,
    /// Path to the .harn file to serve.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct A2aServeArgs {
    /// Socket address to bind the A2A server to. Defaults to loopback; use
    /// `--bind 0.0.0.0:PORT` only behind explicit auth/TLS or a trusted edge.
    #[arg(long, env = "HARN_SERVE_A2A_BIND", value_name = "ADDR")]
    pub bind: Option<SocketAddr>,
    /// Port to bind the A2A server to.
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    /// Public URL advertised in the A2A agent card.
    #[arg(long = "public-url", env = "HARN_SERVE_A2A_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Static API keys accepted via `Authorization: Bearer` or `X-API-Key`.
    #[arg(long = "api-key", env = "HARN_SERVE_API_KEY", value_delimiter = ',')]
    pub api_key: Vec<String>,
    /// Shared secret for HMAC request signing.
    #[arg(long = "hmac-secret", env = "HARN_SERVE_HMAC_SECRET")]
    pub hmac_secret: Option<String>,
    /// Shared secret used to attach an HS256 signature to the agent card.
    #[arg(long = "card-signing-secret", env = "HARN_SERVE_A2A_CARD_SECRET")]
    pub card_signing_secret: Option<String>,
    /// TLS listener mode. Supplying both `--cert` and `--key` implies `pem`.
    #[arg(long = "tls", value_enum, default_value_t = ServeTlsMode::Plain)]
    pub tls: ServeTlsMode,
    /// PEM-encoded certificate chain for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_CERT", value_name = "PATH")]
    pub cert: Option<PathBuf>,
    /// PEM-encoded private key for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_KEY", value_name = "PATH")]
    pub key: Option<PathBuf>,
    /// Where to route `harness.obs.*` spans/metrics/logs.
    #[arg(long = "obs", value_enum, default_value_t = ServeObsMode::Auto)]
    pub obs: ServeObsMode,
    /// Path to the .harn file to serve.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct ApiServeArgs {
    /// Socket address to bind the local Agents API server to.
    #[arg(
        long,
        env = "HARN_SERVE_API_BIND",
        default_value = "127.0.0.1:8787",
        value_name = "ADDR"
    )]
    pub bind: SocketAddr,
    /// Public URL printed and advertised for the local API server.
    #[arg(long = "public-url", env = "HARN_SERVE_API_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Static API keys accepted via `Authorization: Bearer` or `X-API-Key`.
    #[arg(long = "api-key", env = "HARN_SERVE_API_KEY", value_delimiter = ',')]
    pub api_key: Vec<String>,
    /// Shared secret for HMAC request signing.
    #[arg(long = "hmac-secret", env = "HARN_SERVE_HMAC_SECRET")]
    pub hmac_secret: Option<String>,
    /// TLS listener mode. Supplying both `--cert` and `--key` implies `pem`.
    #[arg(long = "tls", value_enum, default_value_t = ServeTlsMode::Plain)]
    pub tls: ServeTlsMode,
    /// PEM-encoded certificate chain for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_CERT", value_name = "PATH")]
    pub cert: Option<PathBuf>,
    /// PEM-encoded private key for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_KEY", value_name = "PATH")]
    pub key: Option<PathBuf>,
    /// Enable LLM trace summaries on shutdown.
    #[arg(
        long = "trace",
        env = "HARN_TRACE",
        action = clap::ArgAction::SetTrue,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub trace: bool,
    /// Where to route `harness.obs.*` spans/metrics/logs.
    #[arg(long = "obs", value_enum, default_value_t = ServeObsMode::Auto)]
    pub obs: ServeObsMode,
    #[command(flatten)]
    pub profile: ProfileArgs,
    /// Path to the `.harn` agent file to serve.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct SiteServeArgs {
    /// Socket address to bind the site server to. Defaults to loopback;
    /// use `--bind 0.0.0.0:PORT` only behind explicit auth/TLS or a
    /// trusted edge.
    #[arg(
        long,
        env = "HARN_SERVE_SITE_BIND",
        default_value = "127.0.0.1:8788",
        value_name = "ADDR"
    )]
    pub bind: SocketAddr,
    /// Public URL printed in the startup banner.
    #[arg(long = "public-url", env = "HARN_SERVE_SITE_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Static API keys accepted via `Authorization: Bearer` or `X-API-Key`.
    #[arg(long = "api-key", env = "HARN_SERVE_API_KEY", value_delimiter = ',')]
    pub api_key: Vec<String>,
    /// Shared secret for HMAC request signing.
    #[arg(long = "hmac-secret", env = "HARN_SERVE_HMAC_SECRET")]
    pub hmac_secret: Option<String>,
    /// TLS listener mode. Supplying both `--cert` and `--key` implies `pem`.
    #[arg(long = "tls", value_enum, default_value_t = ServeTlsMode::Plain)]
    pub tls: ServeTlsMode,
    /// PEM-encoded certificate chain for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_CERT", value_name = "PATH")]
    pub cert: Option<PathBuf>,
    /// PEM-encoded private key for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_KEY", value_name = "PATH")]
    pub key: Option<PathBuf>,
    /// Where to route `harness.obs.*` spans/metrics/logs.
    #[arg(long = "obs", value_enum, default_value_t = ServeObsMode::Auto)]
    pub obs: ServeObsMode,
    /// CIDR range (or bare IP) of a reverse proxy whose `X-Forwarded-For`
    /// / `X-Real-IP` headers may set `req.client_ip`. Repeatable. When
    /// unset, those headers are ignored and `client_ip` is the direct
    /// peer — spoof-proof for servers not behind a trusted proxy.
    #[arg(
        long = "trusted-proxy",
        env = "HARN_SERVE_SITE_TRUSTED_PROXIES",
        value_name = "CIDR",
        value_delimiter = ',',
        value_parser = parse_trusted_proxy
    )]
    pub trusted_proxies: Vec<IpNet>,
    /// Path to the `.harn` file whose routed `pub fn` handlers are served.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct ServeMcpArgs {
    /// Transport to expose for MCP clients.
    #[arg(long, value_enum, default_value_t = McpServeTransport::Stdio)]
    pub transport: McpServeTransport,
    /// Socket address to bind when serving over HTTP.
    #[arg(
        long,
        env = "HARN_SERVE_MCP_BIND",
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
    /// Static API keys accepted over HTTP via `Authorization: Bearer` or `X-API-Key`.
    #[arg(long = "api-key", env = "HARN_SERVE_API_KEY", value_delimiter = ',')]
    pub api_key: Vec<String>,
    /// Shared secret for HMAC request signing on HTTP transports.
    #[arg(long = "hmac-secret", env = "HARN_SERVE_HMAC_SECRET")]
    pub hmac_secret: Option<String>,
    /// TLS listener mode. Supplying both `--cert` and `--key` implies `pem`.
    #[arg(long = "tls", value_enum, default_value_t = ServeTlsMode::Plain)]
    pub tls: ServeTlsMode,
    /// PEM-encoded certificate chain for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_CERT", value_name = "PATH")]
    pub cert: Option<PathBuf>,
    /// PEM-encoded private key for in-process HTTPS termination.
    #[arg(long, env = "HARN_SERVE_KEY", value_name = "PATH")]
    pub key: Option<PathBuf>,
    /// Optional Server Card JSON to advertise (MCP v2.1). Path to a
    /// `.json` file OR an inline JSON string. The card is embedded in
    /// the `initialize` response's `serverInfo.card` field AND exposed
    /// as a static resource at `well-known://mcp-card`.
    #[arg(long = "card", value_name = "PATH_OR_JSON")]
    pub card: Option<String>,
    /// Where to route `harness.obs.*` spans/metrics/logs.
    #[arg(long = "obs", value_enum, default_value_t = ServeObsMode::Auto)]
    pub obs: ServeObsMode,
    /// Path to the `.harn` file whose exported `pub fn` entrypoints are
    /// served. Scripts that instead call `mcp_tools(registry)` /
    /// `mcp_resource(...)` / `mcp_prompt(...)` are detected and served
    /// via the script-driven surface.
    pub file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum McpServeTransport {
    Stdio,
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ServeTlsMode {
    Plain,
    Edge,
    SelfSignedDev,
    Pem,
}
