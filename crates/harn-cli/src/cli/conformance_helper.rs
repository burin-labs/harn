use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ConformanceHelperArgs {
    #[command(subcommand)]
    pub command: ConformanceHelperCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConformanceHelperCommand {
    /// Run a deterministic mock host around `harn run --bridge`.
    #[command(name = "bridge-mock-host")]
    BridgeMockHost(ConformanceHelperBridgeMockHostArgs),
    /// Serve one raw HTTP proxy request and write a state receipt.
    #[command(name = "http-proxy")]
    HttpProxy(ConformanceHelperHttpProxyArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConformanceHelperBridgeMockHostArgs {
    /// Pipeline to execute through `harn run --bridge`.
    pub pipeline: String,
    /// JSON task argument forwarded to `harn run --bridge --arg`.
    #[arg(long = "arg")]
    pub arg: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConformanceHelperHttpProxyArgs {
    /// Directory where the helper writes `port` and `state.json`.
    pub state_dir: String,
    /// Upper bound for waiting on the single conformance request.
    #[arg(long = "timeout-ms", default_value_t = 5_000, hide = true)]
    pub timeout_ms: u64,
}
