use clap::{ArgGroup, Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct MergeCaptainArgs {
    #[command(subcommand)]
    pub command: MergeCaptainCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MergeCaptainCommand {
    /// Run a Merge Captain sweep against a live, mock, or replay backend.
    Run(MergeCaptainRunArgs),
    /// Audit a JSONL transcript against the Merge Captain oracle.
    Audit(MergeCaptainAuditArgs),
    /// Manage a mock-repos playground (real temp git repos + fake GitHub HTTP).
    #[command(subcommand)]
    Mock(MergeCaptainMockCommand),
}

#[derive(Debug, Subcommand)]
pub(crate) enum MergeCaptainMockCommand {
    /// Materialize a playground directory with real bare+working git repos.
    Init(MergeCaptainMockInitArgs),
    /// Apply a named scenario step or a one-off action to the playground state.
    Step(MergeCaptainMockStepArgs),
    /// Show the playground's current PR / check / history state.
    Status(MergeCaptainMockStatusArgs),
    /// Serve the fake GitHub HTTP API backed by the playground.
    Serve(MergeCaptainMockServeArgs),
    /// Idempotently remove the playground directory.
    Cleanup(MergeCaptainMockCleanupArgs),
    /// List the names of built-in scenarios.
    Scenarios,
}

#[derive(Debug, Args)]
pub(crate) struct MergeCaptainMockInitArgs {
    /// Directory to materialize. Created if missing; refuses to overwrite an
    /// existing playground unless `--force` is passed.
    pub dir: String,
    /// Built-in scenario name (see `mock scenarios`). Mutually exclusive
    /// with `--manifest`.
    #[arg(long)]
    pub scenario: Option<String>,
    /// Path to a custom scenario manifest (JSON or YAML). Overrides
    /// `--scenario` when present.
    #[arg(long)]
    pub manifest: Option<String>,
    /// Cleanup any existing playground at DIR first.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("merge_captain_mock_step_target")
        .args(["name", "action"])
        .multiple(false)
        .required(true)
))]
pub(crate) struct MergeCaptainMockStepArgs {
    /// Playground directory.
    pub dir: String,
    /// Named step from the scenario manifest.
    #[arg(long, value_name = "STEP")]
    pub name: Option<String>,
    /// Inline JSON-encoded `ScenarioAction` for one-off mutations.
    #[arg(long, value_name = "JSON")]
    pub action: Option<String>,
    /// Print machine-readable status JSON instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct MergeCaptainMockStatusArgs {
    pub dir: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct MergeCaptainMockServeArgs {
    pub dir: String,
    /// Bind address (e.g. `127.0.0.1:0` for an ephemeral port).
    #[arg(long, default_value = "127.0.0.1:0")]
    pub bind: String,
    /// Print the resolved bind address as JSON to stdout once the server
    /// is ready, then keep serving until SIGINT / SIGTERM.
    #[arg(long)]
    pub print_addr: bool,
}

#[derive(Debug, Args)]
pub(crate) struct MergeCaptainMockCleanupArgs {
    pub dir: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum MergeCaptainBackendKind {
    /// Production connectors and real worktrees.
    Live,
    /// Scenario manifest plus fake backend/playground directory.
    Mock,
    /// Deterministic JSONL transcript fixture.
    Replay,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("merge_captain_run_mode")
        .args(["once", "watch"])
        .multiple(false)
))]
pub(crate) struct MergeCaptainRunArgs {
    /// Backend selector. `mock` and `replay` require BACKEND_ARG.
    #[arg(long, value_enum, default_value_t = MergeCaptainBackendKind::Mock)]
    pub backend: MergeCaptainBackendKind,
    /// Mock playground directory or replay transcript fixture.
    #[arg(value_name = "BACKEND_ARG")]
    pub backend_arg: Option<String>,
    /// Run a single sweep and exit.
    #[arg(long)]
    pub once: bool,
    /// Keep sweeping with backoff. This finite CLI driver caps sweeps via --max-sweeps.
    #[arg(long)]
    pub watch: bool,
    /// Model route/profile identifier to pin in the receipt.
    #[arg(long = "model-route", value_name = "ROUTE")]
    pub model_route: Option<String>,
    /// Timeout or budget tier identifier to pin in the receipt.
    #[arg(long = "timeout-tier", value_name = "TIER")]
    pub timeout_tier: Option<String>,
    /// Write streamed JSONL transcript to this path.
    #[arg(long = "transcript-out", value_name = "PATH")]
    pub transcript_out: Option<String>,
    /// Write the receipt JSON to this path. Defaults under `.harn-runs/merge-captain/`.
    #[arg(long = "receipt-out", value_name = "PATH")]
    pub receipt_out: Option<String>,
    /// Write the machine-readable run summary JSON to this path.
    #[arg(long = "summary-out", value_name = "PATH")]
    pub summary_out: Option<String>,
    /// Maximum sweeps when --watch is selected. Defaults to one for deterministic CLI runs.
    #[arg(long = "max-sweeps", default_value_t = 1)]
    pub max_sweeps: u32,
    /// Backoff between watch sweeps in milliseconds.
    #[arg(long = "watch-backoff-ms", default_value_t = 1000)]
    pub watch_backoff_ms: u64,
    /// Do not stream transcript JSONL to stdout.
    #[arg(long = "no-stdout")]
    pub no_stdout: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum MergeCaptainAuditFormat {
    /// Human-readable summary suitable for terminals.
    Text,
    /// Pretty-printed JSON suitable for CI gates.
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct MergeCaptainAuditArgs {
    /// Path to a `.harn-runs/<session-id>/event_log.jsonl` (or its
    /// parent directory containing rotated `event_log*.jsonl`
    /// files).
    pub transcript: String,
    /// Optional Merge Captain golden fixture (JSON) describing the
    /// scenario's expected state-machine, budgets, and forbidden
    /// actions. Without one, the auditor uses default heuristics.
    #[arg(long, value_name = "PATH")]
    pub golden: Option<String>,
    /// Output format. Defaults to `text`.
    #[arg(long, value_enum, default_value_t = MergeCaptainAuditFormat::Text)]
    pub format: MergeCaptainAuditFormat,
    /// Treat warnings as errors. Useful in CI gates that want to
    /// flip on incomplete-transcript / state-out-of-order findings.
    #[arg(long)]
    pub strict: bool,
}
