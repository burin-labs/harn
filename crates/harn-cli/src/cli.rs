use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "harn",
    about = "The agent harness language",
    version,
    disable_help_subcommand = false,
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Execute a .harn file or an inline expression.
    #[command(long_about = "\
Execute a .harn file or an inline expression.

USAGE
    harn run script.harn
    harn run -e 'println(\"hello\")'
    harn run script.harn -- arg1 arg2   (script reads `argv` as list<string>)

CONCURRENCY
    Harn supports first-class concurrency primitives:
      - spawn { ... }         — launch a task, return a handle
      - parallel each LIST    — concurrent map
      - parallel settle LIST  — concurrent map, collect Ok/Err
      - parallel N            — N-way fan-out
      - with { max_concurrent: N }  — cap in-flight workers
      - channels, retry, select
    https://harnlang.com/concurrency.html

LLM THROTTLING
    Providers can be rate-limited via `rpm:` in harn.toml / providers.toml
    or via `HARN_RATE_LIMIT_<PROVIDER>=N`. Rate limits control throughput
    (RPM); `max_concurrent` on `parallel` caps simultaneous in-flight jobs.

SCRIPTING
    LLM-readable one-pager: https://harnlang.com/docs/llm/harn-quickref.md
    Human cheatsheet:       https://harnlang.com/scripting-cheatsheet.html
    Full docs:              https://harnlang.com/
")]
    Run(RunArgs),
    /// Type-check .harn files or directories without executing them.
    Check(CheckArgs),
    /// Export machine-readable Harn contracts and bundle manifests.
    Contracts(ContractsArgs),
    /// Lint .harn files or directories for common issues.
    Lint(PathTargetsArgs),
    /// Format .harn files or directories.
    Fmt(FmtArgs),
    /// Run user tests or the conformance suite.
    Test(TestArgs),
    /// Scaffold a new project with harn.toml.
    Init(InitArgs),
    /// Scaffold a new project from a starter template.
    New(InitArgs),
    /// Diagnose the local Harn environment and provider setup.
    Doctor(DoctorArgs),
    /// Serve a .harn agent over HTTP using A2A.
    Serve(ServeArgs),
    /// Start the ACP server on stdio.
    Acp(AcpArgs),
    /// Legacy alias: expose a .harn tool bundle as an MCP server on stdio.
    #[command(hide = true, name = "mcp-serve")]
    McpServe(LegacyMcpServeArgs),
    /// Manage remote MCP OAuth credentials and status.
    Mcp(McpArgs),
    /// Watch a .harn file and re-run it on changes.
    Watch(WatchArgs),
    /// Launch the local Harn observability portal.
    Portal(PortalArgs),
    /// Replay and inspect historical trigger dispatches from the event log.
    Trigger(TriggerArgs),
    /// Query and manage trust-graph autonomy state.
    Trust(TrustArgs),
    /// Start the orchestrator process that hosts triggers and connector dispatch.
    Orchestrator(OrchestratorArgs),
    /// Run a pipeline against a Harn-native host module for fast iteration.
    Playground(PlaygroundArgs),
    /// Inspect persisted workflow run records.
    Runs(RunsArgs),
    /// Replay a persisted workflow run record.
    Replay(ReplayArgs),
    /// Evaluate a run record, run directory, or eval manifest.
    Eval(EvalArgs),
    /// Start the interactive REPL.
    Repl,
    /// Benchmark a .harn pipeline over repeated runs.
    Bench(BenchArgs),
    /// Render a .harn file as a Mermaid workflow graph.
    Viz(VizArgs),
    /// Install dependencies declared in harn.toml.
    Install,
    /// Add a dependency to harn.toml.
    Add(AddArgs),
    /// Print resolved metadata for a model alias or model id as JSON.
    ModelInfo(ModelInfoArgs),
    /// Manage and inspect Harn skills (list, inspect, match, install, new).
    Skills(SkillsArgs),
    /// Manage skill provenance: keys, signatures, verification, and trust policy.
    Skill(SkillArgs),
    /// Print the decorated version banner.
    Version,
    /// Regenerate docs/theme/harn-keywords.js from the live lexer + stdlib sets.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-highlight-keywords` or the
    /// `make gen-highlight` target.
    #[command(hide = true, name = "dump-highlight-keywords")]
    DumpHighlightKeywords(DumpHighlightKeywordsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// Print the LLM trace summary after execution.
    #[arg(long)]
    pub trace: bool,
    /// Deny specific builtins as a comma-separated list.
    #[arg(long, conflicts_with = "allow")]
    pub deny: Option<String>,
    /// Allow only the listed builtins as a comma-separated list.
    #[arg(long, conflicts_with = "deny")]
    pub allow: Option<String>,
    /// Evaluate inline Harn code instead of a file.
    #[arg(short = 'e')]
    pub eval: Option<String>,
    /// Extra skill-discovery roots. Repeatable; each path is a
    /// directory of `<name>/SKILL.md` bundles, equivalent to a
    /// single-entry `$HARN_SKILLS_PATH`. Highest-priority layer —
    /// wins ties against every other layer. See `docs/src/skills.md`.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// Replay LLM responses from a JSONL fixture file instead of
    /// calling the configured provider.
    #[arg(
        long = "llm-mock",
        value_name = "PATH",
        conflicts_with = "llm_mock_record"
    )]
    pub llm_mock: Option<String>,
    /// Record executed LLM responses into a JSONL fixture file.
    #[arg(
        long = "llm-mock-record",
        value_name = "PATH",
        conflicts_with = "llm_mock"
    )]
    pub llm_mock_record: Option<String>,
    /// Path to the .harn file to execute.
    pub file: Option<String>,
    /// Positional arguments passed to the pipeline as the global `argv`
    /// list. Place them after a `--` separator: `harn run script.harn -- a b c`.
    // `last = true` alone routes post-`--` tokens into `argv`; combining it
    // with `trailing_var_arg = true` panics at clap runtime.
    #[arg(last = true)]
    pub argv: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    /// Extra host capability schema for preflight validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// Flag unvalidated boundary-API values used in field access.
    #[arg(long = "strict-types")]
    pub strict_types: bool,
    /// Check every `.harn` file under `[workspace].pipelines` in the
    /// nearest `harn.toml`. Positional targets are additive.
    #[arg(long = "workspace")]
    pub workspace: bool,
    /// Downgrade preflight diagnostics to warnings (or suppress them
    /// entirely with `off`). Overrides `[check].preflight_severity`.
    /// Accepted values: `error` (default), `warning`, `off`.
    #[arg(long = "preflight", value_name = "SEVERITY")]
    pub preflight: Option<String>,
    /// One or more .harn files or directories. Optional when `--workspace`
    /// is set.
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsArgs {
    #[command(subcommand)]
    pub command: ContractsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ContractsCommand {
    /// Export builtin registry metadata.
    Builtins(ContractsOutputArgs),
    /// Export the effective host capability manifest used for preflight.
    HostCapabilities(ContractsHostCapabilitiesArgs),
    /// Export a bundle manifest for one or more pipelines and optionally verify it.
    Bundle(ContractsBundleArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ContractsOutputArgs {
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsHostCapabilitiesArgs {
    /// Extra host capability schema to merge into the default manifest.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsBundleArgs {
    /// Extra host capability schema for bundle contract validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// Fail if the selected targets do not pass Harn preflight validation.
    #[arg(long)]
    pub verify: bool,
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PathTargetsArgs {
    /// Automatically apply safe fixes.
    #[arg(long)]
    pub fix: bool,
    /// Force-enable the `require-file-header` rule (overrides harn.toml).
    #[arg(long = "require-file-header")]
    pub require_file_header: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct FmtArgs {
    /// Check formatting without rewriting files.
    #[arg(long)]
    pub check: bool,
    /// Maximum line width before wrapping. Overrides `[fmt] line_width` in harn.toml.
    #[arg(long = "line-width")]
    pub line_width: Option<usize>,
    /// Total width of `// ----` separator bars. Overrides `[fmt] separator_width`.
    #[arg(long = "separator-width")]
    pub separator_width: Option<usize>,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TestArgs {
    /// Only run tests whose names or paths contain this pattern.
    #[arg(long)]
    pub filter: Option<String>,
    /// Write a JUnit XML report to this path.
    #[arg(long)]
    pub junit: Option<String>,
    /// Per-test timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub timeout: u64,
    /// Run user tests concurrently where supported.
    #[arg(long)]
    pub parallel: bool,
    /// Re-run user tests when watched files change.
    #[arg(long)]
    pub watch: bool,
    /// Show per-test timing and detailed failures.
    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue)]
    pub verbose: bool,
    /// Show per-test timing and summary statistics.
    #[arg(long, action = ArgAction::SetTrue)]
    pub timing: bool,
    /// Record LLM fixtures to .harn-fixtures/.
    #[arg(long)]
    pub record: bool,
    /// Replay LLM fixtures from .harn-fixtures/.
    #[arg(long)]
    pub replay: bool,
    /// Extra skill-discovery roots (repeatable). See `harn run
    /// --skill-dir` — applied the same way to user tests and
    /// conformance fixtures so bundled `skills/` dirs are picked up.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// User test path, or `conformance` to target the conformance suite.
    pub target: Option<String>,
    /// Optional file or directory under conformance/ when target is `conformance`.
    pub selection: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// Optional project name to scaffold.
    pub name: Option<String>,
    /// Starter template to scaffold.
    #[arg(long, value_enum, default_value_t = ProjectTemplate::Basic)]
    pub template: ProjectTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProjectTemplate {
    Basic,
    Agent,
    #[value(name = "mcp-server")]
    McpServer,
    Eval,
    #[value(name = "pipeline-lab")]
    PipelineLab,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Skip provider connectivity checks.
    #[arg(long)]
    pub no_network: bool,
}

#[derive(Debug, Args)]
pub(crate) struct VizArgs {
    /// Path to the .harn file to visualize.
    pub file: String,
    /// Optional output path. Defaults to stdout.
    #[arg(short, long)]
    pub output: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct BenchArgs {
    /// Path to the .harn file to benchmark.
    pub file: String,
    /// Number of benchmark iterations to run.
    #[arg(short = 'n', long, default_value_t = 10)]
    pub iterations: usize,
}

#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Port to bind the A2A server to.
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    /// Path to the .harn file to serve.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct AcpArgs {
    /// Optional pipeline to expose through ACP.
    pub pipeline: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct LegacyMcpServeArgs {
    /// Path to the .harn file that defines the MCP surface.
    pub file: String,
    /// Optional Server Card JSON to advertise (MCP v2.1). Path to a
    /// `.json` file OR an inline JSON string. The card is embedded in
    /// the `initialize` response's `serverInfo.card` field AND exposed
    /// as a static resource at `well-known://mcp-card`.
    #[arg(long = "card", value_name = "PATH_OR_JSON")]
    pub card: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum McpServeTransport {
    Stdio,
    Http,
}

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
}

#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// Deny specific builtins as a comma-separated list.
    #[arg(long, conflicts_with = "allow")]
    pub deny: Option<String>,
    /// Allow only the listed builtins as a comma-separated list.
    #[arg(long, conflicts_with = "deny")]
    pub allow: Option<String>,
    /// Path to the .harn file to watch.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct PortalArgs {
    /// Directory containing persisted run records.
    #[arg(long, default_value = ".harn-runs")]
    pub dir: String,
    /// Host interface to bind.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port to serve the portal on.
    #[arg(long, default_value_t = 4721)]
    pub port: u16,
    /// Open the portal in a browser after starting.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub open: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TriggerArgs {
    #[command(subcommand)]
    pub command: TriggerCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TriggerCommand {
    /// Replay a recorded trigger event from the event log.
    Replay(TriggerReplayArgs),
    /// Cancel pending or in-flight trigger dispatches from the event log.
    Cancel(TriggerCancelArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TriggerReplayArgs {
    /// Trigger event id to replay.
    #[arg(required_unless_present = "where_expr", conflicts_with = "where_expr")]
    pub event_id: Option<String>,
    /// Filter replayable trigger records using a Harn expression.
    #[arg(long = "where", value_name = "EXPR", conflicts_with = "event_id")]
    pub where_expr: Option<String>,
    /// Compare the replay outcome to the original stored outcome and emit drift JSON.
    #[arg(long)]
    pub diff: bool,
    /// Resolve the binding version that was active at this historical timestamp.
    #[arg(long = "as-of", value_name = "TIMESTAMP")]
    pub as_of: Option<String>,
    /// Preview which records would be replayed without dispatching them.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit progress lines to stderr while processing bulk operations.
    #[arg(long)]
    pub progress: bool,
    /// Max operations per second for bulk runs. Omit to run without throttling.
    #[arg(long = "rate-limit", value_name = "OPS_PER_SEC")]
    pub rate_limit: Option<f64>,
}

#[derive(Debug, Args)]
pub(crate) struct TriggerCancelArgs {
    /// Trigger event id to cancel.
    #[arg(required_unless_present = "where_expr", conflicts_with = "where_expr")]
    pub event_id: Option<String>,
    /// Filter cancellable trigger records using a Harn expression.
    #[arg(long = "where", value_name = "EXPR", conflicts_with = "event_id")]
    pub where_expr: Option<String>,
    /// Preview which records would be cancelled without writing cancel requests.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit progress lines to stderr while processing bulk operations.
    #[arg(long)]
    pub progress: bool,
    /// Max operations per second for bulk runs. Omit to run without throttling.
    #[arg(long = "rate-limit", value_name = "OPS_PER_SEC")]
    pub rate_limit: Option<f64>,
}

#[derive(Debug, Args)]
pub(crate) struct TrustArgs {
    #[command(subcommand)]
    pub command: TrustCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TrustCommand {
    /// Query trust records from the event log.
    Query(TrustQueryArgs),
    /// Promote an agent to a higher autonomy tier.
    Promote(TrustPromoteArgs),
    /// Demote an agent to a lower autonomy tier.
    Demote(TrustDemoteArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustTierArg {
    Shadow,
    Suggest,
    ActWithApproval,
    ActAuto,
}

impl From<TrustTierArg> for harn_vm::AutonomyTier {
    fn from(value: TrustTierArg) -> Self {
        match value {
            TrustTierArg::Shadow => harn_vm::AutonomyTier::Shadow,
            TrustTierArg::Suggest => harn_vm::AutonomyTier::Suggest,
            TrustTierArg::ActWithApproval => harn_vm::AutonomyTier::ActWithApproval,
            TrustTierArg::ActAuto => harn_vm::AutonomyTier::ActAuto,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustOutcomeArg {
    Success,
    Failure,
    Denied,
    Timeout,
}

impl From<TrustOutcomeArg> for harn_vm::TrustOutcome {
    fn from(value: TrustOutcomeArg) -> Self {
        match value {
            TrustOutcomeArg::Success => harn_vm::TrustOutcome::Success,
            TrustOutcomeArg::Failure => harn_vm::TrustOutcome::Failure,
            TrustOutcomeArg::Denied => harn_vm::TrustOutcome::Denied,
            TrustOutcomeArg::Timeout => harn_vm::TrustOutcome::Timeout,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct TrustQueryArgs {
    /// Filter by agent id.
    #[arg(long)]
    pub agent: Option<String>,
    /// Filter by action class.
    #[arg(long)]
    pub action: Option<String>,
    /// Include records at or after this RFC3339/unix timestamp.
    #[arg(long)]
    pub since: Option<String>,
    /// Include records at or before this RFC3339/unix timestamp.
    #[arg(long)]
    pub until: Option<String>,
    /// Filter by autonomy tier.
    #[arg(long, value_enum)]
    pub tier: Option<TrustTierArg>,
    /// Filter by trust outcome.
    #[arg(long, value_enum)]
    pub outcome: Option<TrustOutcomeArg>,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Summarize records per agent.
    #[arg(long)]
    pub summary: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TrustPromoteArgs {
    /// Agent id to promote.
    pub agent: String,
    /// Target autonomy tier.
    #[arg(long, value_enum)]
    pub to: TrustTierArg,
}

#[derive(Debug, Args)]
pub(crate) struct TrustDemoteArgs {
    /// Agent id to demote.
    pub agent: String,
    /// Target autonomy tier.
    #[arg(long, value_enum)]
    pub to: TrustTierArg,
    /// Reason for the demotion.
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorArgs {
    #[command(subcommand)]
    pub command: OrchestratorCommand,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct OrchestratorLocalArgs {
    /// Path to the root manifest to load. Container deployments often mount
    /// this as `/etc/harn/triggers.toml`.
    #[arg(
        long,
        visible_alias = "manifest",
        env = "HARN_ORCHESTRATOR_MANIFEST",
        default_value = "harn.toml",
        value_name = "PATH"
    )]
    pub config: PathBuf,
    /// Directory used for EventLog data and orchestrator state snapshots.
    #[arg(
        long = "state-dir",
        env = "HARN_ORCHESTRATOR_STATE_DIR",
        default_value = ".harn/orchestrator",
        value_name = "PATH"
    )]
    pub state_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OrchestratorCommand {
    /// Load manifests, initialize registries, and idle until shutdown.
    Serve(OrchestratorServeArgs),
    /// Inspect orchestrator state.
    Inspect(OrchestratorInspectArgs),
    /// Inject a synthetic event for a specific binding.
    Fire(OrchestratorFireArgs),
    /// Replay orchestrator events.
    Replay(OrchestratorReplayArgs),
    /// Resume a paused HITL escalation by accepting its request id.
    Resume(OrchestratorResumeArgs),
    /// Inspect the dead-letter queue.
    Dlq(OrchestratorDlqArgs),
    /// Inspect trigger and dispatch queues.
    Queue(OrchestratorQueueArgs),
    /// Replay stranded inbox envelopes explicitly.
    Recover(OrchestratorRecoverArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorServeArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Socket address the HTTP listener will bind to.
    #[arg(
        long,
        visible_alias = "listen",
        env = "HARN_ORCHESTRATOR_LISTEN",
        default_value = "127.0.0.1:8080",
        value_name = "ADDR"
    )]
    pub bind: SocketAddr,
    /// PEM-encoded certificate chain for HTTPS termination.
    #[arg(long, env = "HARN_ORCHESTRATOR_CERT", value_name = "PATH")]
    pub cert: Option<PathBuf>,
    /// PEM-encoded private key for HTTPS termination.
    #[arg(long, env = "HARN_ORCHESTRATOR_KEY", value_name = "PATH")]
    pub key: Option<PathBuf>,
    /// Seconds to wait for connector and dispatcher drain before forcing shutdown.
    #[arg(long = "shutdown-timeout", default_value_t = 30, value_name = "SECS")]
    pub shutdown_timeout: u64,
    /// Maximum number of pump items to process during graceful shutdown.
    #[arg(long = "drain-max-items", value_name = "COUNT")]
    pub drain_max_items: Option<usize>,
    /// Seconds to wait for each pump drain before truncating remaining backlog.
    #[arg(long = "drain-deadline", value_name = "SECS")]
    pub drain_deadline: Option<u64>,
    /// Runtime role to boot. Multi-tenant is a stub for now.
    #[arg(
        long,
        env = "HARN_ORCHESTRATOR_ROLE",
        value_enum,
        default_value_t = crate::commands::orchestrator::role::OrchestratorRole::SingleTenant
    )]
    pub role: crate::commands::orchestrator::role::OrchestratorRole,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorInspectArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorFireArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Binding id to fire.
    pub binding_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorReplayArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Previously recorded event id to replay.
    pub event_id: String,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorResumeArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// HITL request id to resume.
    pub event_id: String,
    /// Reviewer/actor recorded on the acceptance event.
    #[arg(long, default_value = "manual")]
    pub reviewer: String,
    /// Optional human-readable resume reason.
    #[arg(long)]
    pub reason: Option<String>,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorDlqArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// List pending DLQ entries.
    #[arg(
        long,
        default_value_t = false,
        action = ArgAction::SetTrue,
        conflicts_with_all = ["replay", "discard"]
    )]
    pub list: bool,
    /// Replay the DLQ entry identified by id.
    #[arg(long, value_name = "ID", conflicts_with = "discard")]
    pub replay: Option<String>,
    /// Discard the DLQ entry identified by id.
    #[arg(long, value_name = "ID", conflicts_with = "replay")]
    pub discard: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorQueueArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    #[command(subcommand)]
    pub command: Option<OrchestratorQueueCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OrchestratorQueueCommand {
    /// List worker queues plus the existing dispatcher/inbox summary.
    Ls(OrchestratorQueueLsArgs),
    /// Claim and process every ready job on one worker queue.
    Drain(OrchestratorQueueDrainArgs),
    /// Drop all currently unclaimed jobs from one worker queue.
    Purge(OrchestratorQueuePurgeArgs),
}

#[derive(Debug, Args, Default)]
pub(crate) struct OrchestratorQueueLsArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorQueueDrainArgs {
    /// Queue name to drain.
    pub queue: String,
    /// Consumer id recorded on queue claims/responses. Defaults to a generated local id.
    #[arg(long, value_name = "ID")]
    pub consumer_id: Option<String>,
    /// Claim TTL before another consumer may re-claim an in-flight job.
    #[arg(long = "claim-ttl", value_name = "DURATION", value_parser = parse_duration_arg, default_value = "5m")]
    pub claim_ttl: StdDuration,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorQueuePurgeArgs {
    /// Queue name to purge.
    pub queue: String,
    /// Confirm that the ready jobs should actually be purged.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub confirm: bool,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorRecoverArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Minimum stranded-envelope age required before replay/listing.
    #[arg(long = "envelope-age", value_name = "DURATION", value_parser = parse_duration_arg)]
    pub envelope_age: StdDuration,
    /// List stranded envelopes without replaying them.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub dry_run: bool,
    /// Confirm that stranded envelopes should actually be replayed.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PlaygroundArgs {
    /// Host module exporting the capabilities the script expects.
    #[arg(long, default_value = "host.harn")]
    pub host: String,
    /// Pipeline entrypoint to execute.
    #[arg(long, default_value = "pipeline.harn")]
    pub script: String,
    /// Runtime task string exposed via `runtime_task()`.
    #[arg(long)]
    pub task: Option<String>,
    /// Provider/model override as `provider:model`.
    #[arg(long)]
    pub llm: Option<String>,
    /// Replay LLM responses from a JSONL fixture file instead of
    /// calling the configured provider.
    #[arg(
        long = "llm-mock",
        value_name = "PATH",
        conflicts_with = "llm_mock_record"
    )]
    pub llm_mock: Option<String>,
    /// Record executed LLM responses into a JSONL fixture file.
    #[arg(
        long = "llm-mock-record",
        value_name = "PATH",
        conflicts_with = "llm_mock"
    )]
    pub llm_mock_record: Option<String>,
    /// Re-run when the script or host module changes.
    #[arg(long)]
    pub watch: bool,
}

fn parse_duration_arg(raw: &str) -> Result<StdDuration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("duration cannot be empty".to_string());
    }

    let (digits, unit) = raw
        .chars()
        .position(|ch| !ch.is_ascii_digit())
        .map(|index| raw.split_at(index))
        .ok_or_else(|| {
            "duration must include a unit suffix like ms, s, m, h, d, or w".to_string()
        })?;
    if digits.is_empty() || unit.is_empty() {
        return Err("duration must be formatted like 30s, 5m, 2h, or 7d".to_string());
    }

    let value = digits
        .parse::<u64>()
        .map_err(|error| format!("invalid duration '{raw}': {error}"))?;
    match unit {
        "ms" => Ok(StdDuration::from_millis(value)),
        "s" => Ok(StdDuration::from_secs(value)),
        "m" => Ok(StdDuration::from_secs(value.saturating_mul(60))),
        "h" => Ok(StdDuration::from_secs(value.saturating_mul(60 * 60))),
        "d" => Ok(StdDuration::from_secs(value.saturating_mul(60 * 60 * 24))),
        "w" => Ok(StdDuration::from_secs(
            value.saturating_mul(60 * 60 * 24 * 7),
        )),
        _ => Err(format!(
            "unsupported duration unit '{unit}'; expected ms, s, m, h, d, or w"
        )),
    }
}

#[derive(Debug, Args)]
pub(crate) struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunsCommand {
    /// Inspect a persisted run record and optionally diff it against another.
    Inspect(RunsInspectArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunsInspectArgs {
    /// Path to the run record JSON file.
    pub path: String,
    /// Optional baseline run record to diff against.
    #[arg(long)]
    pub compare: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReplayArgs {
    /// Path to the run record JSON file.
    pub path: String,
}

#[derive(Debug, Args)]
pub(crate) struct EvalArgs {
    /// Run record path, run directory, or eval manifest path.
    pub path: String,
    /// Optional baseline run record for diffing.
    #[arg(long)]
    pub compare: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct DumpHighlightKeywordsArgs {
    /// Path to the generated keyword file (relative to the repo root).
    #[arg(long, default_value = "docs/theme/harn-keywords.js")]
    pub output: String,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift between the highlighter and the
    /// lexer/stdlib.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Dependency name to add to harn.toml.
    pub name: String,
    /// Git URL for a remote dependency.
    #[arg(long, conflicts_with = "path")]
    pub git: Option<String>,
    /// Git tag to pin for a remote dependency.
    #[arg(long)]
    pub tag: Option<String>,
    /// Local path for a path dependency.
    #[arg(long, conflicts_with = "git")]
    pub path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ModelInfoArgs {
    /// Model alias or provider-native model id.
    pub model: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillsCommand {
    /// Show resolved skills in priority order, with collision warnings.
    List(SkillsListArgs),
    /// Dump the resolved SKILL.md plus bundled files and metadata for one skill.
    Inspect(SkillsInspectArgs),
    /// Run the metadata matcher against a prompt and show ranked candidates.
    #[command(name = "match")]
    Match(SkillsMatchArgs),
    /// Resolve a git ref or local path into `.harn/skills-cache/` so the layered resolver picks it up.
    Install(SkillsInstallArgs),
    /// Scaffold a new SKILL.md bundle under `.harn/skills/<name>/`.
    New(SkillsNewArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SkillsListArgs {
    /// Emit newline-delimited JSON (one record per skill) instead of a table.
    #[arg(long)]
    pub json: bool,
    /// Optional path used to anchor manifest and project discovery (defaults to cwd).
    #[arg(long = "from", value_name = "PATH")]
    pub from: Option<String>,
    /// Extra skill-discovery roots (repeatable). Same as `harn run --skill-dir`.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// Include shadowed (lower-priority) entries as well as the winners.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsInspectArgs {
    /// Skill id to inspect (e.g. `deploy` or `acme/ops/deploy`).
    pub name: String,
    /// Emit JSON instead of a human-readable dump.
    #[arg(long)]
    pub json: bool,
    /// Optional path used to anchor manifest and project discovery (defaults to cwd).
    #[arg(long = "from", value_name = "PATH")]
    pub from: Option<String>,
    /// Extra skill-discovery roots (repeatable).
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsMatchArgs {
    /// Prompt text to score every discovered skill against.
    pub query: String,
    /// Top-N matches to display. Default 5.
    #[arg(long, default_value_t = 5)]
    pub top_n: usize,
    /// Emit JSON instead of a ranked table.
    #[arg(long)]
    pub json: bool,
    /// Simulate working-file path globs (repeatable).
    #[arg(long = "working-file", value_name = "PATH")]
    pub working_files: Vec<String>,
    /// Optional path used to anchor manifest and project discovery.
    #[arg(long = "from", value_name = "PATH")]
    pub from: Option<String>,
    /// Extra skill-discovery roots (repeatable).
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsInstallArgs {
    /// Spec: a git URL, `owner/repo`, or a local filesystem path.
    pub spec: String,
    /// Optional human name / directory label for the cached install.
    #[arg(long)]
    pub name: Option<String>,
    /// Git tag or branch to pin (only applies to git specs).
    #[arg(long)]
    pub tag: Option<String>,
    /// Optional namespace prefix registered with the installed skills.
    #[arg(long)]
    pub namespace: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsNewArgs {
    /// Skill identifier (used as the directory name and default `name:`).
    pub name: String,
    /// One-line `short:` card for the SKILL.md frontmatter.
    #[arg(long)]
    pub description: Option<String>,
    /// Override the destination directory. Defaults to `.harn/skills/<name>/`.
    #[arg(long = "dir", value_name = "PATH")]
    pub dir: Option<String>,
    /// Overwrite any existing files at the destination.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillCommand {
    /// Manage Ed25519 signing keys for skill provenance.
    Key(SkillKeyArgs),
    /// Sign a skill manifest and emit `<path>.sig`.
    Sign(SkillSignArgs),
    /// Verify a skill manifest against the trusted signer set.
    Verify(SkillVerifyArgs),
    /// Manage the local trusted signer registry.
    Trust(SkillTrustArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SkillKeyArgs {
    #[command(subcommand)]
    pub command: SkillKeyCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillKeyCommand {
    /// Generate an Ed25519 keypair and write PEM files to disk.
    Generate(SkillKeyGenerateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SkillKeyGenerateArgs {
    /// Path for the private-key PEM. The public key is written to `<path>.pub`.
    #[arg(long, value_name = "PATH")]
    pub out: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillSignArgs {
    /// Path to the skill manifest to sign (typically `SKILL.md`).
    pub skill: String,
    /// Path to the private-key PEM generated by `harn skill key generate`.
    #[arg(long, value_name = "PATH")]
    pub key: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillVerifyArgs {
    /// Path to the skill manifest to verify.
    pub skill: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillTrustArgs {
    #[command(subcommand)]
    pub command: SkillTrustCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillTrustCommand {
    /// Import a trusted signer from a PEM file or URL.
    Add(SkillTrustAddArgs),
    /// List the trusted signer fingerprints currently installed locally.
    List(SkillTrustListArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SkillTrustAddArgs {
    /// PEM source for the public key. Accepts a local path or URL.
    #[arg(long = "from", value_name = "URL|FILE")]
    pub from: String,
}

#[derive(Debug, Args, Default)]
pub(crate) struct SkillTrustListArgs {}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration as StdDuration;

    use super::{
        Cli, Command, McpCommand, OrchestratorCommand, ProjectTemplate, RunsCommand, SkillCommand,
        SkillKeyCommand, SkillTrustCommand, SkillsCommand, TriggerCommand, TrustCommand,
        TrustOutcomeArg, TrustTierArg,
    };
    use clap::Parser;

    #[test]
    fn test_parses_conformance_target_selection() {
        let cli = Cli::parse_from([
            "harn",
            "test",
            "conformance",
            "tests/worktree_runtime.harn",
            "--verbose",
        ]);

        let Command::Test(args) = cli.command.unwrap() else {
            panic!("expected test command");
        };
        assert_eq!(args.target.as_deref(), Some("conformance"));
        assert_eq!(
            args.selection.as_deref(),
            Some("tests/worktree_runtime.harn")
        );
        assert!(args.verbose);
    }

    #[test]
    fn test_run_rejects_deny_allow_conflict() {
        let err = Cli::try_parse_from([
            "harn",
            "run",
            "--deny",
            "read_file",
            "--allow",
            "exec",
            "main.harn",
        ])
        .unwrap_err();

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn test_parses_run_llm_mock_flags() {
        let cli = Cli::parse_from(["harn", "run", "--llm-mock", "fixtures.jsonl", "main.harn"]);

        let Command::Run(args) = cli.command.unwrap() else {
            panic!("expected run command");
        };
        assert_eq!(args.llm_mock.as_deref(), Some("fixtures.jsonl"));
        assert_eq!(args.llm_mock_record, None);

        let cli = Cli::parse_from(["harn", "run", "--llm-mock-record", "out.jsonl", "main.harn"]);

        let Command::Run(args) = cli.command.unwrap() else {
            panic!("expected run command");
        };
        assert_eq!(args.llm_mock_record.as_deref(), Some("out.jsonl"));
        assert_eq!(args.llm_mock, None);
    }

    #[test]
    fn test_parses_mcp_login_flags() {
        let cli = Cli::parse_from([
            "harn",
            "mcp",
            "login",
            "notion",
            "--url",
            "https://example.com/mcp",
            "--client-id",
            "abc",
        ]);

        let Command::Mcp(args) = cli.command.unwrap() else {
            panic!("expected mcp command");
        };
        let McpCommand::Login(login) = args.command else {
            panic!("expected mcp login");
        };
        assert_eq!(login.target.as_deref(), Some("notion"));
        assert_eq!(login.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(login.client_id.as_deref(), Some("abc"));
    }

    #[test]
    fn test_parses_mcp_serve_flags() {
        let cli = Cli::parse_from([
            "harn",
            "mcp",
            "serve",
            "--config",
            "workspace/harn.toml",
            "--state-dir",
            "state/orchestrator",
            "--transport",
            "http",
            "--bind",
            "127.0.0.1:9000",
            "--path",
            "/rpc",
            "--sse-path",
            "/events",
            "--messages-path",
            "/legacy/messages",
        ]);

        let Command::Mcp(args) = cli.command.unwrap() else {
            panic!("expected mcp command");
        };
        let McpCommand::Serve(serve) = args.command else {
            panic!("expected mcp serve");
        };
        assert_eq!(serve.local.config, PathBuf::from("workspace/harn.toml"));
        assert_eq!(serve.local.state_dir, PathBuf::from("state/orchestrator"));
        assert_eq!(serve.transport, crate::cli::McpServeTransport::Http);
        assert_eq!(serve.bind.to_string(), "127.0.0.1:9000");
        assert_eq!(serve.path, "/rpc");
        assert_eq!(serve.sse_path, "/events");
        assert_eq!(serve.messages_path, "/legacy/messages");
    }

    #[test]
    fn test_parses_runs_inspect_compare() {
        let cli = Cli::parse_from([
            "harn",
            "runs",
            "inspect",
            "run.json",
            "--compare",
            "baseline.json",
        ]);

        let Command::Runs(args) = cli.command.unwrap() else {
            panic!("expected runs command");
        };
        let RunsCommand::Inspect(inspect) = args.command;
        assert_eq!(inspect.path, "run.json");
        assert_eq!(inspect.compare.as_deref(), Some("baseline.json"));
    }

    #[test]
    fn test_parses_trigger_replay_flags() {
        let cli = Cli::parse_from([
            "harn",
            "trigger",
            "replay",
            "trigger_evt_123",
            "--diff",
            "--as-of",
            "2026-04-19T18:00:00Z",
        ]);

        let Command::Trigger(args) = cli.command.unwrap() else {
            panic!("expected trigger command");
        };
        let TriggerCommand::Replay(replay) = args.command else {
            panic!("expected trigger replay");
        };
        assert_eq!(replay.event_id.as_deref(), Some("trigger_evt_123"));
        assert!(replay.diff);
        assert_eq!(replay.as_of.as_deref(), Some("2026-04-19T18:00:00Z"));
        assert!(replay.where_expr.is_none());
    }

    #[test]
    fn test_parses_trigger_bulk_cancel_flags() {
        let cli = Cli::parse_from([
            "harn",
            "trigger",
            "cancel",
            "--where",
            "event.payload.tenant == 'acme' AND attempt.handler == 'handlers::risky'",
            "--dry-run",
            "--progress",
            "--rate-limit",
            "4",
        ]);

        let Command::Trigger(args) = cli.command.unwrap() else {
            panic!("expected trigger command");
        };
        let TriggerCommand::Cancel(cancel) = args.command else {
            panic!("expected trigger cancel");
        };
        assert!(cancel.event_id.is_none());
        assert_eq!(
            cancel.where_expr.as_deref(),
            Some("event.payload.tenant == 'acme' AND attempt.handler == 'handlers::risky'")
        );
        assert!(cancel.dry_run);
        assert!(cancel.progress);
        assert_eq!(cancel.rate_limit, Some(4.0));
    }

    #[test]
    fn test_parses_trust_query_flags() {
        let cli = Cli::parse_from([
            "harn",
            "trust",
            "query",
            "--agent",
            "github-triage-bot",
            "--action",
            "github.issue.opened",
            "--since",
            "2026-04-19T18:00:00Z",
            "--until",
            "2026-04-19T19:00:00Z",
            "--tier",
            "act-auto",
            "--outcome",
            "success",
            "--json",
            "--summary",
        ]);

        let Command::Trust(args) = cli.command.unwrap() else {
            panic!("expected trust command");
        };
        let TrustCommand::Query(query) = args.command else {
            panic!("expected trust query");
        };
        assert_eq!(query.agent.as_deref(), Some("github-triage-bot"));
        assert_eq!(query.action.as_deref(), Some("github.issue.opened"));
        assert_eq!(query.since.as_deref(), Some("2026-04-19T18:00:00Z"));
        assert_eq!(query.until.as_deref(), Some("2026-04-19T19:00:00Z"));
        assert!(matches!(query.tier, Some(TrustTierArg::ActAuto)));
        assert!(matches!(query.outcome, Some(TrustOutcomeArg::Success)));
        assert!(query.json);
        assert!(query.summary);
    }

    #[test]
    fn test_parses_trust_demote_flags() {
        let cli = Cli::parse_from([
            "harn",
            "trust",
            "demote",
            "github-triage-bot",
            "--to",
            "shadow",
            "--reason",
            "unexpected mutation",
        ]);

        let Command::Trust(args) = cli.command.unwrap() else {
            panic!("expected trust command");
        };
        let TrustCommand::Demote(demote) = args.command else {
            panic!("expected trust demote");
        };
        assert_eq!(demote.agent, "github-triage-bot");
        assert!(matches!(demote.to, TrustTierArg::Shadow));
        assert_eq!(demote.reason, "unexpected mutation");
    }

    #[test]
    fn test_parses_portal_flags() {
        let cli = Cli::parse_from([
            "harn", "portal", "--dir", "runs", "--host", "0.0.0.0", "--port", "4900", "--open",
            "false",
        ]);

        let Command::Portal(args) = cli.command.unwrap() else {
            panic!("expected portal command");
        };
        assert_eq!(args.dir, "runs");
        assert_eq!(args.host, "0.0.0.0");
        assert_eq!(args.port, 4900);
        assert!(!args.open);
    }

    #[test]
    fn test_parses_orchestrator_serve_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "serve",
            "--config",
            "workspace/harn.toml",
            "--state-dir",
            "state/orchestrator",
            "--bind",
            "0.0.0.0:8080",
            "--cert",
            "tls/cert.pem",
            "--key",
            "tls/key.pem",
            "--shutdown-timeout",
            "45",
            "--drain-max-items",
            "256",
            "--drain-deadline",
            "9",
            "--role",
            "single-tenant",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Serve(serve) = args.command else {
            panic!("expected orchestrator serve");
        };
        assert_eq!(serve.local.config, PathBuf::from("workspace/harn.toml"));
        assert_eq!(serve.local.state_dir, PathBuf::from("state/orchestrator"));
        assert_eq!(serve.bind.to_string(), "0.0.0.0:8080");
        assert_eq!(serve.cert, Some(PathBuf::from("tls/cert.pem")));
        assert_eq!(serve.key, Some(PathBuf::from("tls/key.pem")));
        assert_eq!(serve.shutdown_timeout, 45);
        assert_eq!(serve.drain_max_items, Some(256));
        assert_eq!(serve.drain_deadline, Some(9));
        assert_eq!(
            serve.role,
            crate::commands::orchestrator::role::OrchestratorRole::SingleTenant
        );
    }

    #[test]
    fn test_parses_orchestrator_serve_container_aliases() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "serve",
            "--manifest",
            "/etc/harn/triggers.toml",
            "--state-dir",
            "/var/lib/harn/state",
            "--listen",
            "0.0.0.0:8080",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Serve(serve) = args.command else {
            panic!("expected orchestrator serve");
        };
        assert_eq!(serve.local.config, PathBuf::from("/etc/harn/triggers.toml"));
        assert_eq!(serve.local.state_dir, PathBuf::from("/var/lib/harn/state"));
        assert_eq!(serve.bind.to_string(), "0.0.0.0:8080");
    }

    #[test]
    fn test_parses_orchestrator_inspect_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "inspect",
            "--config",
            "workspace/harn.toml",
            "--state-dir",
            "state/orchestrator",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Inspect(inspect) = args.command else {
            panic!("expected orchestrator inspect");
        };
        assert_eq!(inspect.local.config, PathBuf::from("workspace/harn.toml"));
        assert_eq!(inspect.local.state_dir, PathBuf::from("state/orchestrator"));
        assert!(!inspect.json);
    }

    #[test]
    fn test_parses_orchestrator_fire_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "fire",
            "github-new-issue",
            "--config",
            "workspace/harn.toml",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Fire(fire) = args.command else {
            panic!("expected orchestrator fire");
        };
        assert_eq!(fire.binding_id, "github-new-issue");
        assert_eq!(fire.local.config, PathBuf::from("workspace/harn.toml"));
    }

    #[test]
    fn test_parses_orchestrator_replay_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "replay",
            "trigger_evt_123",
            "--state-dir",
            "state/orchestrator",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Replay(replay) = args.command else {
            panic!("expected orchestrator replay");
        };
        assert_eq!(replay.event_id, "trigger_evt_123");
        assert_eq!(replay.local.state_dir, PathBuf::from("state/orchestrator"));
        assert!(!replay.json);
    }

    #[test]
    fn test_parses_orchestrator_dlq_replay_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "dlq",
            "--replay",
            "dlq_123",
            "--config",
            "workspace/harn.toml",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Dlq(dlq) = args.command else {
            panic!("expected orchestrator dlq");
        };
        assert_eq!(dlq.replay.as_deref(), Some("dlq_123"));
        assert!(dlq.discard.is_none());
        assert!(!dlq.list);
        assert_eq!(dlq.local.config, PathBuf::from("workspace/harn.toml"));
        assert!(!dlq.json);
    }

    #[test]
    fn test_parses_orchestrator_json_flags() {
        let inspect_cli = Cli::parse_from(["harn", "orchestrator", "inspect", "--json"]);
        let Command::Orchestrator(inspect_args) = inspect_cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Inspect(inspect) = inspect_args.command else {
            panic!("expected orchestrator inspect");
        };
        assert!(inspect.json);

        let replay_cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "replay",
            "trigger_evt_123",
            "--json",
        ]);
        let Command::Orchestrator(replay_args) = replay_cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Replay(replay) = replay_args.command else {
            panic!("expected orchestrator replay");
        };
        assert!(replay.json);

        let dlq_cli = Cli::parse_from(["harn", "orchestrator", "dlq", "--json", "--list"]);
        let Command::Orchestrator(dlq_args) = dlq_cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Dlq(dlq) = dlq_args.command else {
            panic!("expected orchestrator dlq");
        };
        assert!(dlq.json);
        assert!(dlq.list);
    }

    #[test]
    fn test_parses_orchestrator_resume_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "resume",
            "hitl_escalation_trigger_evt_123_1",
            "--reviewer",
            "ops-lead",
            "--reason",
            "manual escalation ack",
            "--json",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Resume(resume) = args.command else {
            panic!("expected orchestrator resume");
        };
        assert_eq!(resume.event_id, "hitl_escalation_trigger_evt_123_1");
        assert_eq!(resume.reviewer, "ops-lead");
        assert_eq!(resume.reason.as_deref(), Some("manual escalation ack"));
        assert!(resume.json);
    }

    #[test]
    fn test_parses_orchestrator_queue_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "queue",
            "--state-dir",
            "state/orchestrator",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Queue(queue) = args.command else {
            panic!("expected orchestrator queue");
        };
        assert_eq!(queue.local.state_dir, PathBuf::from("state/orchestrator"));
        assert!(queue.command.is_none());
    }

    #[test]
    fn test_parses_orchestrator_queue_drain_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "queue",
            "--state-dir",
            "state/orchestrator",
            "drain",
            "triage",
            "--consumer-id",
            "worker-a",
            "--claim-ttl",
            "30s",
            "--json",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Queue(queue) = args.command else {
            panic!("expected orchestrator queue");
        };
        let Some(OrchestratorQueueCommand::Drain(drain)) = queue.command else {
            panic!("expected orchestrator queue drain");
        };
        assert_eq!(queue.local.state_dir, PathBuf::from("state/orchestrator"));
        assert_eq!(drain.queue, "triage");
        assert_eq!(drain.consumer_id.as_deref(), Some("worker-a"));
        assert_eq!(drain.claim_ttl, StdDuration::from_secs(30));
        assert!(drain.json);
    }

    #[test]
    fn test_parses_orchestrator_queue_purge_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "queue",
            "--state-dir",
            "state/orchestrator",
            "purge",
            "triage",
            "--confirm",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Queue(queue) = args.command else {
            panic!("expected orchestrator queue");
        };
        let Some(OrchestratorQueueCommand::Purge(purge)) = queue.command else {
            panic!("expected orchestrator queue purge");
        };
        assert_eq!(queue.local.state_dir, PathBuf::from("state/orchestrator"));
        assert_eq!(purge.queue, "triage");
        assert!(purge.confirm);
    }

    #[test]
    fn test_parses_orchestrator_recover_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "recover",
            "--config",
            "workspace/harn.toml",
            "--state-dir",
            "state/orchestrator",
            "--envelope-age",
            "15m",
            "--dry-run",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Recover(recover) = args.command else {
            panic!("expected orchestrator recover");
        };
        assert_eq!(recover.local.config, PathBuf::from("workspace/harn.toml"));
        assert_eq!(recover.local.state_dir, PathBuf::from("state/orchestrator"));
        assert_eq!(recover.envelope_age, StdDuration::from_secs(15 * 60));
        assert!(recover.dry_run);
        assert!(!recover.yes);
    }

    #[test]
    fn test_parses_new_template() {
        let cli = Cli::parse_from(["harn", "new", "review-bot", "--template", "agent"]);

        let Command::New(args) = cli.command.unwrap() else {
            panic!("expected new command");
        };
        assert_eq!(args.name.as_deref(), Some("review-bot"));
        assert_eq!(args.template, ProjectTemplate::Agent);
    }

    #[test]
    fn test_parses_pipeline_lab_template() {
        let cli = Cli::parse_from([
            "harn",
            "new",
            "pipeline-lab-demo",
            "--template",
            "pipeline-lab",
        ]);

        let Command::New(args) = cli.command.unwrap() else {
            panic!("expected new command");
        };
        assert_eq!(args.template, ProjectTemplate::PipelineLab);
    }

    #[test]
    fn test_parses_playground_args() {
        let cli = Cli::parse_from([
            "harn",
            "playground",
            "--host",
            "examples/playground/host.harn",
            "--script",
            "examples/playground/echo.harn",
            "--task",
            "hi",
            "--llm",
            "ollama:qwen2.5-coder:latest",
            "--watch",
        ]);

        let Command::Playground(args) = cli.command.unwrap() else {
            panic!("expected playground command");
        };
        assert_eq!(args.host, "examples/playground/host.harn");
        assert_eq!(args.script, "examples/playground/echo.harn");
        assert_eq!(args.task.as_deref(), Some("hi"));
        assert_eq!(args.llm.as_deref(), Some("ollama:qwen2.5-coder:latest"));
        assert_eq!(args.llm_mock, None);
        assert_eq!(args.llm_mock_record, None);
        assert!(args.watch);
    }

    #[test]
    fn test_parses_playground_llm_mock_flags() {
        let cli = Cli::parse_from([
            "harn",
            "playground",
            "--llm-mock",
            "fixtures.jsonl",
            "--host",
            "host.harn",
        ]);

        let Command::Playground(args) = cli.command.unwrap() else {
            panic!("expected playground command");
        };
        assert_eq!(args.llm_mock.as_deref(), Some("fixtures.jsonl"));
        assert_eq!(args.llm_mock_record, None);

        let cli = Cli::parse_from(["harn", "playground", "--llm-mock-record", "recorded.jsonl"]);

        let Command::Playground(args) = cli.command.unwrap() else {
            panic!("expected playground command");
        };
        assert_eq!(args.llm_mock, None);
        assert_eq!(args.llm_mock_record.as_deref(), Some("recorded.jsonl"));
    }

    #[test]
    fn test_parses_doctor_flags() {
        let cli = Cli::parse_from(["harn", "doctor", "--no-network"]);

        let Command::Doctor(args) = cli.command.unwrap() else {
            panic!("expected doctor command");
        };
        assert!(args.no_network);
    }

    #[test]
    fn test_parses_viz_args() {
        let cli = Cli::parse_from(["harn", "viz", "main.harn", "--output", "graph.mmd"]);

        let Command::Viz(args) = cli.command.unwrap() else {
            panic!("expected viz command");
        };
        assert_eq!(args.file, "main.harn");
        assert_eq!(args.output.as_deref(), Some("graph.mmd"));
    }

    #[test]
    fn test_parses_bench_args() {
        let cli = Cli::parse_from(["harn", "bench", "main.harn", "--iterations", "25"]);

        let Command::Bench(args) = cli.command.unwrap() else {
            panic!("expected bench command");
        };
        assert_eq!(args.file, "main.harn");
        assert_eq!(args.iterations, 25);
    }

    #[test]
    fn test_parses_skills_subcommands() {
        let cli = Cli::parse_from(["harn", "skills", "list", "--json", "--all"]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::List(list) = args.command else {
            panic!("expected skills list");
        };
        assert!(list.json);
        assert!(list.all);

        let cli = Cli::parse_from(["harn", "skills", "match", "deploy the app", "--top-n", "3"]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::Match(matcher) = args.command else {
            panic!("expected skills match");
        };
        assert_eq!(matcher.query, "deploy the app");
        assert_eq!(matcher.top_n, 3);

        let cli = Cli::parse_from([
            "harn",
            "skills",
            "install",
            "https://example.com/acme/harn-skills.git",
            "--tag",
            "v1.0",
            "--namespace",
            "acme",
        ]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::Install(install) = args.command else {
            panic!("expected skills install");
        };
        assert_eq!(install.tag.as_deref(), Some("v1.0"));
        assert_eq!(install.namespace.as_deref(), Some("acme"));

        let cli = Cli::parse_from([
            "harn",
            "skills",
            "new",
            "deploy",
            "--description",
            "Ship things",
        ]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::New(new_args) = args.command else {
            panic!("expected skills new");
        };
        assert_eq!(new_args.name, "deploy");
        assert_eq!(new_args.description.as_deref(), Some("Ship things"));
    }

    #[test]
    fn test_parses_skill_provenance_subcommands() {
        let cli = Cli::parse_from(["harn", "skill", "key", "generate", "--out", "signer.pem"]);
        let Command::Skill(args) = cli.command.unwrap() else {
            panic!("expected skill command");
        };
        let SkillCommand::Key(key_args) = args.command else {
            panic!("expected skill key");
        };
        let SkillKeyCommand::Generate(generate) = key_args.command;
        assert_eq!(generate.out, "signer.pem");

        let cli = Cli::parse_from(["harn", "skill", "sign", "SKILL.md", "--key", "signer.pem"]);
        let Command::Skill(args) = cli.command.unwrap() else {
            panic!("expected skill command");
        };
        let SkillCommand::Sign(sign) = args.command else {
            panic!("expected skill sign");
        };
        assert_eq!(sign.skill, "SKILL.md");
        assert_eq!(sign.key, "signer.pem");

        let cli = Cli::parse_from(["harn", "skill", "verify", "SKILL.md"]);
        let Command::Skill(args) = cli.command.unwrap() else {
            panic!("expected skill command");
        };
        let SkillCommand::Verify(verify) = args.command else {
            panic!("expected skill verify");
        };
        assert_eq!(verify.skill, "SKILL.md");

        let cli = Cli::parse_from([
            "harn",
            "skill",
            "trust",
            "add",
            "--from",
            "https://example.com/signer.pub",
        ]);
        let Command::Skill(args) = cli.command.unwrap() else {
            panic!("expected skill command");
        };
        let SkillCommand::Trust(trust) = args.command else {
            panic!("expected skill trust");
        };
        let SkillTrustCommand::Add(add) = trust.command else {
            panic!("expected skill trust add");
        };
        assert_eq!(add.from, "https://example.com/signer.pub");

        let cli = Cli::parse_from(["harn", "skill", "trust", "list"]);
        let Command::Skill(args) = cli.command.unwrap() else {
            panic!("expected skill command");
        };
        let SkillCommand::Trust(trust) = args.command else {
            panic!("expected skill trust");
        };
        assert!(matches!(trust.command, SkillTrustCommand::List(_)));
    }

    #[test]
    fn test_parses_model_info_args() {
        let cli = Cli::parse_from(["harn", "model-info", "tog-gemma4-31b"]);

        let Command::ModelInfo(args) = cli.command.unwrap() else {
            panic!("expected model-info command");
        };
        assert_eq!(args.model, "tog-gemma4-31b");
    }
}
