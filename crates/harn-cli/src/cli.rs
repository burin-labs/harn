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
    LLM-readable one-pager: https://harnlang.com/docs/llm/harn-quickref.html
    Human cheatsheet:       https://harnlang.com/scripting-cheatsheet.html
    Full docs:              https://harnlang.com/
")]
    Run(RunArgs),
    /// Type-check .harn files or directories without executing them.
    Check(CheckArgs),
    /// Explain the CFG path behind an invariant violation for one handler.
    Explain(ExplainArgs),
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
    /// Scaffold a new project, package, or connector from a starter template.
    New(NewArgs),
    /// Diagnose the local Harn environment and provider setup.
    Doctor(DoctorArgs),
    /// Register outbound connector resources with a provider.
    Connect(ConnectArgs),
    /// Validate pure-Harn connector packages against the connector contract.
    Connector(ConnectorArgs),
    /// Serve a Harn workflow over a transport adapter.
    Serve(ServeArgs),
    /// Manage remote MCP OAuth credentials and status.
    Mcp(McpArgs),
    /// Watch a .harn file and re-run it on changes.
    Watch(WatchArgs),
    /// Launch the local Harn observability portal.
    Portal(PortalArgs),
    /// Replay and inspect historical trigger dispatches from the event log.
    Trigger(TriggerArgs),
    /// Import third-party eval traces into replayable Harn fixtures.
    Trace(TraceArgs),
    /// Mine repeated traces into a reviewable deterministic Harn workflow candidate.
    Crystallize(CrystallizeArgs),
    /// Query and manage trust-graph autonomy state.
    Trust(TrustArgs),
    /// Query and verify trust-graph autonomy state.
    #[command(name = "trust-graph")]
    TrustGraph(TrustArgs),
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
    Install(InstallArgs),
    /// Add a dependency to harn.toml.
    Add(AddArgs),
    /// Refresh one or more dependency lock entries.
    Update(UpdateArgs),
    /// Remove a dependency from harn.toml and harn.lock.
    Remove(RemoveArgs),
    /// Resolve dependencies and write harn.lock without materializing packages.
    Lock,
    /// Manage Harn package caches and integrity verification.
    Package(PackageArgs),
    /// Prepare a package for publication. Real registry submission is not yet enabled.
    Publish(PublishArgs),
    /// List and inspect durable agent persona manifests.
    Persona(PersonaArgs),
    /// Print resolved metadata for a model alias or model id as JSON.
    ModelInfo(ModelInfoArgs),
    /// Print the provider/model catalog Harn loaded as JSON.
    ProviderCatalog(ProviderCatalogArgs),
    /// Probe a provider's /models endpoint and optionally verify a served model.
    ProviderReady(ProviderReadyArgs),
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
    /// Regenerate docs/llm/harn-triggers-quickref.md from the live trigger provider catalog.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-trigger-quickref` or the
    /// `make gen-trigger-quickref` target.
    #[command(hide = true, name = "dump-trigger-quickref")]
    DumpTriggerQuickref(DumpTriggerQuickrefArgs),
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
    /// Evaluate `@invariant(...)` annotations and fail on violations.
    #[arg(long = "invariants")]
    pub invariants: bool,
    /// One or more .harn files or directories. Optional when `--workspace`
    /// is set.
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    /// The invariant name to explain, e.g. `fs.writes`.
    #[arg(long = "invariant", value_name = "NAME")]
    pub invariant: String,
    /// The handler / function / tool / pipeline name to inspect.
    pub function: String,
    /// Path to the `.harn` source file.
    pub file: String,
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
    /// Agents Protocol Harness base URL when running `harn test agents-conformance`.
    #[arg(long = "target", value_name = "URL")]
    pub agents_target: Option<String>,
    /// Bearer API key for `harn test agents-conformance`.
    #[arg(long = "api-key", env = "HARN_AGENTS_CONFORMANCE_API_KEY")]
    pub agents_api_key: Option<String>,
    /// Restrict `harn test agents-conformance` to one category. Repeatable or comma-separated.
    #[arg(long = "category", value_name = "NAME")]
    pub agents_category: Vec<String>,
    /// Emit the agents conformance leaderboard-shaped JSON report to stdout.
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,
    /// Write the agents conformance leaderboard-shaped JSON report to this path.
    #[arg(long = "json-out", value_name = "PATH")]
    pub json_out: Option<String>,
    /// Existing workspace id to reuse for agents conformance probes.
    #[arg(long = "workspace-id", value_name = "ID")]
    pub agents_workspace_id: Option<String>,
    /// Existing session id to reuse for agents conformance probes.
    #[arg(long = "session-id", value_name = "ID")]
    pub agents_session_id: Option<String>,
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
    /// Record then replay each selected pipeline and assert deterministic output.
    #[arg(long)]
    pub determinism: bool,
    /// Run eval packs declared by the nearest package manifest.
    #[arg(long)]
    pub evals: bool,
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
    Package,
    Connector,
}

#[derive(Debug, Args)]
pub(crate) struct NewArgs {
    /// Project name, or `package` / `connector` when using `harn new package NAME`.
    pub first: Option<String>,
    /// Package or connector name when the first positional argument is a kind.
    pub second: Option<String>,
    /// Starter template to scaffold.
    #[arg(long, value_enum)]
    pub template: Option<ProjectTemplate>,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Skip provider connectivity checks.
    #[arg(long)]
    pub no_network: bool,
}

#[derive(Debug, Args)]
#[command(
    after_long_help = "Registered provider commands:\n  harn connect <provider> [OPTIONS]\n\nIf <provider> is not one of the built-in subcommands, Harn reads OAuth metadata from the nearest harn.toml [[providers]] entry."
)]
pub(crate) struct ConnectArgs {
    /// Show authenticated connector tokens known to the local keyring.
    #[arg(long)]
    pub list: bool,
    /// Remove locally stored OAuth material for a provider.
    #[arg(long, value_name = "PROVIDER")]
    pub revoke: Option<String>,
    /// Force-refresh locally stored OAuth material for a provider.
    #[arg(long, value_name = "PROVIDER")]
    pub refresh: Option<String>,
    /// Run the generic OAuth 2.1 flow for a provider/resource pair.
    #[arg(long, value_names = ["PROVIDER", "URL"], num_args = 2)]
    pub generic: Vec<String>,
    /// Emit machine-readable JSON for list/connect management operations.
    #[arg(long)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<ConnectCommand>,
}

#[derive(Debug, Args)]
pub(crate) struct ConnectorArgs {
    #[command(subcommand)]
    pub command: ConnectorCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectorCommand {
    /// Check a pure-Harn connector package against connector contract v1.
    Check(ConnectorCheckArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectorCheckArgs {
    /// Package directory, harn.toml, or file under the package to check.
    pub package: String,
    /// Restrict the check to one provider id. Repeatable.
    #[arg(long = "provider", value_name = "ID")]
    pub providers: Vec<String>,
    /// Run poll bindings long enough to execute the first poll_tick.
    #[arg(long = "run-poll-tick")]
    pub run_poll_tick: bool,
    /// Emit the check report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectCommand {
    /// Capture GitHub App installation metadata and optional app secrets.
    Github(ConnectGithubArgs),
    /// Authorize Linear using OAuth, or register a webhook when --url is supplied.
    Linear(ConnectLinearArgs),
    /// Authorize Slack using OAuth and store connector tokens.
    Slack(ConnectOAuthArgs),
    /// Authorize Notion using OAuth and store connector tokens.
    Notion(ConnectOAuthArgs),
    /// Run the generic OAuth 2.1 flow for any compliant provider.
    Generic(ConnectGenericArgs),
    /// Authorize a provider registered in harn.toml [[providers]] metadata.
    #[command(external_subcommand)]
    Provider(Vec<String>),
}

#[derive(Debug, Args)]
pub(crate) struct ConnectGithubArgs {
    /// GitHub App slug used to build the install URL.
    #[arg(long)]
    pub app_slug: Option<String>,
    /// GitHub App id. Required when storing a private key.
    #[arg(long)]
    pub app_id: Option<String>,
    /// Existing installation id. Skips waiting for the browser callback.
    #[arg(long)]
    pub installation_id: Option<String>,
    /// Override the GitHub App installation URL.
    #[arg(long)]
    pub install_url: Option<String>,
    /// Loopback callback URL. Port 0 binds a random localhost port.
    #[arg(long, default_value = "http://127.0.0.1:0/gh-install-callback")]
    pub redirect_uri: String,
    /// PEM private-key file to store as github/app-<app_id>/private-key.
    #[arg(long)]
    pub private_key_file: Option<PathBuf>,
    /// Inline webhook signing secret to store as github/webhook-secret.
    #[arg(long, conflicts_with = "webhook_secret_file")]
    pub webhook_secret: Option<String>,
    /// Webhook signing secret file to store as github/webhook-secret.
    #[arg(long, conflicts_with = "webhook_secret")]
    pub webhook_secret_file: Option<PathBuf>,
    /// Do not open the system browser; print the URL instead.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ConnectLinearArgs {
    /// Public HTTPS URL that Linear should deliver webhook events to.
    #[arg(long)]
    pub url: Option<String>,
    /// Optional path to an explicit `harn.toml`. Defaults to the nearest manifest from cwd.
    #[arg(long)]
    pub config: Option<String>,
    /// Linear team id for a team-scoped webhook.
    #[arg(long, conflicts_with = "all_public_teams")]
    pub team_id: Option<String>,
    /// Register the webhook for all public teams instead of one team.
    #[arg(long, conflicts_with = "team_id")]
    pub all_public_teams: bool,
    /// Optional display label for the Linear webhook.
    #[arg(long)]
    pub label: Option<String>,
    /// Override the Linear GraphQL endpoint, mainly for tests and self-hosted proxies.
    #[arg(long)]
    pub api_base_url: Option<String>,
    /// Inline Linear personal API key.
    #[arg(long, conflicts_with_all = ["api_key_secret", "access_token", "access_token_secret"])]
    pub api_key: Option<String>,
    /// Secret id containing a Linear personal API key (`namespace/name[@version]`).
    #[arg(long, conflicts_with_all = ["api_key", "access_token", "access_token_secret"])]
    pub api_key_secret: Option<String>,
    /// Inline OAuth access token.
    #[arg(long, conflicts_with_all = ["api_key", "api_key_secret", "access_token_secret"])]
    pub access_token: Option<String>,
    /// Secret id containing an OAuth access token (`namespace/name[@version]`).
    #[arg(long, conflicts_with_all = ["api_key", "api_key_secret", "access_token"])]
    pub access_token_secret: Option<String>,
    /// Explicit OAuth client ID for guided Linear authorization.
    #[arg(long = "client-id")]
    pub client_id: Option<String>,
    /// Explicit OAuth client secret for guided Linear authorization.
    #[arg(long = "client-secret")]
    pub client_secret: Option<String>,
    /// Requested OAuth scope string for guided Linear authorization.
    #[arg(long = "scope")]
    pub scope: Option<String>,
    /// Optional OAuth resource indicator.
    #[arg(long = "resource")]
    pub resource: Option<String>,
    /// Override the authorization endpoint.
    #[arg(long = "auth-url")]
    pub auth_url: Option<String>,
    /// Override the token endpoint.
    #[arg(long = "token-url")]
    pub token_url: Option<String>,
    /// Override token endpoint auth method: none, client_secret_post, or client_secret_basic.
    #[arg(long = "token-auth-method")]
    pub token_auth_method: Option<String>,
    /// Loopback callback URL. Port 0 binds a random localhost port.
    #[arg(
        long = "redirect-uri",
        default_value = "http://127.0.0.1:0/oauth/callback"
    )]
    pub redirect_uri: String,
    /// Do not open the system browser; print the URL instead.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectOAuthArgs {
    /// Explicit OAuth client ID.
    #[arg(long = "client-id")]
    pub client_id: Option<String>,
    /// Explicit OAuth client secret.
    #[arg(long = "client-secret")]
    pub client_secret: Option<String>,
    /// Requested OAuth scope string.
    #[arg(long = "scope")]
    pub scope: Option<String>,
    /// Optional OAuth resource indicator.
    #[arg(long = "resource")]
    pub resource: Option<String>,
    /// Override the authorization endpoint.
    #[arg(long = "auth-url")]
    pub auth_url: Option<String>,
    /// Override the token endpoint.
    #[arg(long = "token-url")]
    pub token_url: Option<String>,
    /// Override token endpoint auth method: none, client_secret_post, or client_secret_basic.
    #[arg(long = "token-auth-method")]
    pub token_auth_method: Option<String>,
    /// Loopback callback URL. Port 0 binds a random localhost port.
    #[arg(
        long = "redirect-uri",
        default_value = "http://127.0.0.1:0/oauth/callback"
    )]
    pub redirect_uri: String,
    /// Do not open the system browser; print the URL instead.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectGenericArgs {
    /// Provider name used for local secret ids.
    pub provider: String,
    /// Protected resource URL. Used for OAuth discovery and resource indicators.
    pub url: String,
    #[command(flatten)]
    pub oauth: ConnectOAuthArgs,
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
    #[command(subcommand)]
    pub command: ServeCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ServeCommand {
    /// Serve a .harn agent over stdio using ACP.
    Acp(ServeAcpArgs),
    /// Serve a .harn agent over HTTP using A2A.
    A2a(A2aServeArgs),
    /// Serve a `.harn` file as an MCP server. Exposes either exported
    /// `pub fn` entrypoints (recommended) or, when the script registers
    /// tools/resources/prompts via `mcp_tools(...)` / `mcp_resource(...)`
    /// / `mcp_prompt(...)`, that script-driven surface over stdio.
    Mcp(ServeMcpArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ServeAcpArgs {
    /// Path to the .harn file to serve.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct A2aServeArgs {
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
    /// Path to the .harn file to serve.
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
    /// as a static resource at `well-known://mcp-card`. Honored when
    /// the script uses the legacy `mcp_tools(...)` registration surface.
    #[arg(long = "card", value_name = "PATH_OR_JSON")]
    pub card: Option<String>,
    /// Path to the `.harn` file whose exported `pub fn` entrypoints are
    /// served. Scripts that instead call `mcp_tools(registry)` /
    /// `mcp_resource(...)` / `mcp_prompt(...)` are detected and served
    /// via the script-driven surface (over stdio).
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
pub(crate) struct TraceArgs {
    #[command(subcommand)]
    pub command: TraceCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TraceCommand {
    /// Convert a generic JSONL trace into a replayable `--llm-mock` fixture.
    Import(TraceImportArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TraceImportArgs {
    /// Source JSONL trace file with `{prompt,response,tool_calls}` records.
    #[arg(long = "trace-file", value_name = "PATH")]
    pub trace_file: String,
    /// Optional trace id to filter when the source file contains multiple traces.
    #[arg(long = "trace-id", value_name = "ID")]
    pub trace_id: Option<String>,
    /// Output path for the generated replay fixture JSONL.
    #[arg(long = "output", value_name = "PATH")]
    pub output: String,
}

#[derive(Debug, Args)]
pub(crate) struct CrystallizeArgs {
    /// Directory containing crystallization trace JSON files or persisted run records.
    #[arg(long = "from", value_name = "TRACE_DIR")]
    pub from: String,
    /// Output path for the generated .harn workflow candidate.
    #[arg(long = "out", value_name = "WORKFLOW")]
    pub out: String,
    /// Output path for the machine-readable crystallization report.
    #[arg(long = "report", value_name = "REPORT_JSON")]
    pub report: String,
    /// Optional output path for a minimal eval pack manifest.
    #[arg(long = "eval-pack", value_name = "HARN_EVAL_TOML")]
    pub eval_pack: Option<String>,
    /// Minimum number of traces that must contain the repeated sequence.
    #[arg(long = "min-examples", default_value_t = 2)]
    pub min_examples: usize,
    /// Override the generated workflow name.
    #[arg(long = "workflow-name", value_name = "NAME")]
    pub workflow_name: Option<String>,
    /// Package name to place in promotion metadata.
    #[arg(long = "package-name", value_name = "NAME")]
    pub package_name: Option<String>,
    /// Author to include in promotion metadata.
    #[arg(long = "author", value_name = "USER")]
    pub author: Option<String>,
    /// Approver to include in promotion metadata.
    #[arg(long = "approver", value_name = "USER")]
    pub approver: Option<String>,
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
    /// Verify the trust graph hash chain.
    VerifyChain(TrustVerifyChainArgs),
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
    /// Limit results to the newest N matching records.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Group results into trace buckets instead of returning a flat list.
    #[arg(long)]
    pub grouped_by_trace: bool,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Summarize records per agent.
    #[arg(long)]
    pub summary: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TrustVerifyChainArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
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
    /// Generate and run a cloud deploy for a manifest-driven orchestrator.
    Deploy(OrchestratorDeployArgs),
    /// Request a hot reload from a running orchestrator.
    Reload(OrchestratorReloadArgs),
    /// Inspect orchestrator state.
    Inspect(OrchestratorInspectArgs),
    /// Summarize trigger analytics and LLM cost/token telemetry.
    Stats(OrchestratorStatsArgs),
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
    /// Manage multi-tenant orchestrator tenants.
    Tenant(OrchestratorTenantArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorTenantArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    #[command(subcommand)]
    pub command: OrchestratorTenantCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OrchestratorTenantCommand {
    /// Create a tenant and print its initial API key.
    Create(OrchestratorTenantCreateArgs),
    /// List registered tenants.
    Ls(OrchestratorTenantLsArgs),
    /// Suspend a tenant while preserving state.
    Suspend(OrchestratorTenantSuspendArgs),
    /// Delete a tenant and remove its state.
    Delete(OrchestratorTenantDeleteArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorTenantCreateArgs {
    /// Tenant id to provision.
    pub id: String,
    /// Daily tenant budget in USD.
    #[arg(long = "daily-cost-usd", value_name = "USD")]
    pub daily_cost_usd: Option<f64>,
    /// Hourly tenant budget in USD.
    #[arg(long = "hourly-cost-usd", value_name = "USD")]
    pub hourly_cost_usd: Option<f64>,
    /// Tenant ingest rate limit.
    #[arg(long = "ingest-per-minute", value_name = "N")]
    pub ingest_per_minute: Option<u32>,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Default)]
pub(crate) struct OrchestratorTenantLsArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorTenantSuspendArgs {
    /// Tenant id to suspend.
    pub id: String,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorTenantDeleteArgs {
    /// Tenant id to delete.
    pub id: String,
    /// Confirm destructive tenant state removal.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub confirm: bool,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
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
    /// Maximum outstanding items admitted by each topic pump.
    #[arg(
        long = "pump-max-outstanding",
        env = "HARN_ORCHESTRATOR_PUMP_MAX_OUTSTANDING",
        value_name = "COUNT"
    )]
    pub pump_max_outstanding: Option<usize>,
    /// Watch the manifest file and trigger reloads on changes.
    #[arg(long)]
    pub watch: bool,
    /// Log output format for orchestrator process logs.
    #[arg(
        long = "log-format",
        env = "HARN_ORCHESTRATOR_LOG_FORMAT",
        value_enum,
        default_value_t = OrchestratorLogFormat::Text
    )]
    pub log_format: OrchestratorLogFormat,
    /// Runtime role to boot. Multi-tenant is a stub for now.
    #[arg(
        long,
        env = "HARN_ORCHESTRATOR_ROLE",
        value_enum,
        default_value_t = crate::commands::orchestrator::role::OrchestratorRole::SingleTenant
    )]
    pub role: crate::commands::orchestrator::role::OrchestratorRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OrchestratorLogFormat {
    Text,
    Pretty,
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorDeployArgs {
    /// Cloud provider target.
    #[arg(long, value_enum)]
    pub provider: OrchestratorDeployProvider,
    /// Path to the root manifest to validate and deploy.
    #[arg(
        long,
        visible_alias = "config",
        env = "HARN_ORCHESTRATOR_MANIFEST",
        default_value = "harn.toml",
        value_name = "PATH"
    )]
    pub manifest: PathBuf,
    /// Service/app name used in generated provider templates.
    #[arg(long, default_value = "harn-orchestrator", value_name = "NAME")]
    pub name: String,
    /// Container image to deploy, or the tag to build/push when --build is set.
    #[arg(
        long,
        default_value = "ghcr.io/burin-labs/harn:latest",
        value_name = "IMAGE"
    )]
    pub image: String,
    /// Directory where provider deploy bundles are written.
    #[arg(long = "deploy-dir", default_value = "deploy", value_name = "DIR")]
    pub deploy_dir: PathBuf,
    /// Internal HTTP port the orchestrator listens on in the container.
    #[arg(long, default_value_t = 8080, value_name = "PORT")]
    pub port: u16,
    /// Persistent data mount path inside the container.
    #[arg(long = "data-dir", default_value = "/data", value_name = "PATH")]
    pub data_dir: String,
    /// Persistent disk or volume size in GiB where the provider supports it.
    #[arg(long = "disk-size-gb", default_value_t = 10, value_name = "GB")]
    pub disk_size_gb: u16,
    /// Graceful shutdown timeout, passed through to orchestrator serve.
    #[arg(long = "shutdown-timeout", default_value_t = 30, value_name = "SECS")]
    pub shutdown_timeout: u64,
    /// Optional provider region for templates and deploy commands that support it.
    #[arg(long, value_name = "REGION")]
    pub region: Option<String>,
    /// Render service id/name to redeploy with `render deploys create`.
    #[arg(long = "render-service", value_name = "SERVICE")]
    pub render_service: Option<String>,
    /// Railway service id/name for variable sync and deploy targeting.
    #[arg(long = "railway-service", value_name = "SERVICE")]
    pub railway_service: Option<String>,
    /// Railway environment id/name for variable sync and deploy targeting.
    #[arg(long = "railway-environment", value_name = "ENV")]
    pub railway_environment: Option<String>,
    /// Build and push the deploy image before running the provider deploy.
    #[arg(long)]
    pub build: bool,
    /// Build locally without pushing when --build is set.
    #[arg(long = "no-push")]
    pub no_push: bool,
    /// Extra runtime environment variable to include and sync, as KEY=VALUE.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Extra runtime secret to sync through the provider CLI, as KEY=VALUE.
    #[arg(long = "secret", value_name = "KEY=VALUE")]
    pub secret: Vec<String>,
    /// Skip provider CLI secret synchronization.
    #[arg(long = "no-secret-sync")]
    pub no_secret_sync: bool,
    /// Generate files and print commands without invoking provider CLIs.
    #[arg(long)]
    pub dry_run: bool,
    /// Print the generated provider spec to stdout.
    #[arg(long)]
    pub print: bool,
    /// Health URL to probe after the deploy command completes.
    #[arg(long = "health-url", value_name = "URL")]
    pub health_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OrchestratorDeployProvider {
    Render,
    Fly,
    Railway,
}

impl OrchestratorDeployProvider {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Render => "render",
            Self::Fly => "fly",
            Self::Railway => "railway",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct OrchestratorReloadArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Explicit admin base URL. Defaults to the running listener URL from the state snapshot.
    #[arg(long = "admin-url", value_name = "URL")]
    pub admin_url: Option<String>,
    /// Request timeout in seconds.
    #[arg(long, default_value_t = 10, value_name = "SECS")]
    pub timeout: u64,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
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
pub(crate) struct OrchestratorStatsArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Rolling window to summarize.
    #[arg(long = "window", value_name = "DURATION", value_parser = parse_duration_arg, default_value = "24h")]
    pub window: StdDuration,
    /// Number of hot triggers/providers to show.
    #[arg(long = "top", default_value_t = 10, value_name = "N")]
    pub top: usize,
    /// Restrict analytics to one tenant id.
    #[arg(long = "tenant", value_name = "TENANT")]
    pub tenant: Option<String>,
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
    /// Run a pipeline twice and compare the baseline against this structural experiment.
    #[arg(long = "structural-experiment")]
    pub structural_experiment: Option<String>,
    /// Replay LLM responses from a JSONL fixture file when `path` is a `.harn` pipeline.
    #[arg(
        long = "llm-mock",
        value_name = "PATH",
        conflicts_with = "llm_mock_record"
    )]
    pub llm_mock: Option<String>,
    /// Record executed LLM responses into a JSONL fixture file when `path` is a `.harn` pipeline.
    #[arg(
        long = "llm-mock-record",
        value_name = "PATH",
        conflicts_with = "llm_mock"
    )]
    pub llm_mock_record: Option<String>,
    /// Positional arguments forwarded to `harn run <pipeline.harn> -- ...` when
    /// `path` is a pipeline file and `--structural-experiment` is set.
    #[arg(last = true)]
    pub argv: Vec<String>,
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
pub(crate) struct DumpTriggerQuickrefArgs {
    /// Path to the generated trigger quickref file (relative to the repo root).
    #[arg(long, default_value = "docs/llm/harn-triggers-quickref.md")]
    pub output: String,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift between the quickref and the
    /// runtime trigger provider catalog.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// Fail if harn.lock would need to change.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub frozen: bool,
    /// Alias for --frozen, intended for CI and production installs.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub locked: bool,
    /// Do not fetch from package sources; use only harn.lock and the local cache.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue, conflicts_with = "refetch")]
    pub offline: bool,
    /// Force refetching one dependency (or every dependency with `--refetch all`).
    #[arg(long, value_name = "ALIAS|all")]
    pub refetch: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Git URL/ref spec, local package path, or dependency name in the legacy `--git/--path` form.
    pub name_or_spec: String,
    /// Override the dependency alias written under `[dependencies]`.
    #[arg(long)]
    pub alias: Option<String>,
    /// Git URL for a remote dependency.
    #[arg(long, conflicts_with = "path")]
    pub git: Option<String>,
    /// Deprecated alias for `--rev` on the legacy `--git` form.
    #[arg(long, conflicts_with_all = ["rev", "branch"])]
    pub tag: Option<String>,
    /// Git rev to pin for a remote dependency.
    #[arg(long, conflicts_with = "branch")]
    pub rev: Option<String>,
    /// Git branch to track in the manifest; the lockfile still pins a commit.
    #[arg(long, conflicts_with_all = ["rev", "tag"])]
    pub branch: Option<String>,
    /// Local path for a path dependency.
    #[arg(long, conflicts_with = "git")]
    pub path: Option<String>,
    /// Package registry index URL or path for registry-name dependencies.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Refresh every dependency instead of one alias.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub all: bool,
    /// Refresh only this dependency alias.
    pub alias: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
    /// Dependency alias to remove from harn.toml and harn.lock.
    pub alias: String,
}

#[derive(Debug, Args)]
pub(crate) struct PackageArgs {
    #[command(subcommand)]
    pub command: PackageCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageCommand {
    /// Search the package registry index.
    Search(PackageSearchArgs),
    /// Show package registry metadata for one package.
    Info(PackageInfoArgs),
    /// Validate a package manifest and publish readiness.
    Check(PackageCheckArgs),
    /// Build an inspectable package artifact directory.
    Pack(PackagePackArgs),
    /// Generate package API docs from exported Harn symbols.
    Docs(PackageDocsArgs),
    /// Inspect, clean, and verify the shared package cache.
    Cache(PackageCacheArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PackageCheckArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackagePackArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Output artifact directory. Defaults to .harn/dist/<name>-<version>.
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,
    /// Validate and print the file list without writing the artifact.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageDocsArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Output Markdown file. Defaults to docs/api.md.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// Verify the output file already matches generated docs.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageSearchArgs {
    /// Search query. Omit to list all registry packages.
    pub query: Option<String>,
    /// Package registry index URL or path.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of a tab-separated table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageInfoArgs {
    /// Registry package name, optionally with @version.
    pub name: String,
    /// Package registry index URL or path.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of human-readable metadata.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PublishArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Required until registry submission support lands.
    #[arg(long)]
    pub dry_run: bool,
    /// Package registry index URL or path to report as the submission target.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageCacheArgs {
    #[command(subcommand)]
    pub command: PackageCacheCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageCacheCommand {
    /// List cached git package entries.
    List,
    /// Remove cached git package entries not referenced by harn.lock, or every entry with --all.
    Clean(PackageCacheCleanArgs),
    /// Verify cached package contents against harn.lock.
    Verify(PackageCacheVerifyArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PersonaArgs {
    #[command(subcommand)]
    pub command: PersonaCommand,
    /// Explicit harn.toml path or directory. Defaults to nearest harn.toml from cwd.
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// Directory used for durable persona runtime state and event-log data.
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        default_value = ".harn/personas"
    )]
    pub state_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PersonaCommand {
    /// List personas declared in the resolved harn.toml.
    List(PersonaListArgs),
    /// Inspect one persona from the resolved harn.toml.
    Inspect(PersonaInspectArgs),
    /// Query durable persona lifecycle, lease, budget, and queue status.
    Status(PersonaStatusArgs),
    /// Pause a persona; matching events queue until resume drains them.
    Pause(PersonaControlArgs),
    /// Resume a persona and drain queued events once under leases.
    Resume(PersonaControlArgs),
    /// Disable a persona; matching events are recorded as dead-lettered.
    Disable(PersonaControlArgs),
    /// Fire a synthetic schedule tick for a persona.
    Tick(PersonaTickArgs),
    /// Fire a synthetic external trigger envelope for a persona.
    Trigger(PersonaTriggerArgs),
    /// Record an expensive-work budget receipt for a persona.
    Spend(PersonaSpendArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PersonaListArgs {
    /// Emit a stable JSON array instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaInspectArgs {
    /// Persona name to inspect.
    pub name: String,
    /// Emit stable JSON for Harn Cloud, Burin Code, or other hosts.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaStatusArgs {
    /// Persona name to query.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic budget windows. When
    /// omitted, falls back to the current UTC wall clock.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON for Harn Cloud, Burin Code, or other hosts.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaControlArgs {
    /// Persona name to control.
    pub name: String,
    /// RFC3339 timestamp to use as "now" for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON after applying the control.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaTickArgs {
    /// Persona name to wake from its schedule binding.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Estimated expensive-work cost for budget enforcement.
    #[arg(long, default_value_t = 0.0)]
    pub cost_usd: f64,
    /// Estimated token count for budget enforcement.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaTriggerArgs {
    /// Persona name to wake from an external trigger.
    pub name: String,
    /// Provider name, for example github, linear, slack, or webhook.
    #[arg(long)]
    pub provider: String,
    /// Provider event kind, for example pull_request, check_run, issue, or message.
    #[arg(long)]
    pub kind: String,
    /// Normalized metadata as key=value. Repeat for multiple fields.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Estimated expensive-work cost for budget enforcement.
    #[arg(long, default_value_t = 0.0)]
    pub cost_usd: f64,
    /// Estimated token count for budget enforcement.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaSpendArgs {
    /// Persona name to charge.
    pub name: String,
    /// Cost in USD to record.
    #[arg(long)]
    pub cost_usd: f64,
    /// Tokens to record.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageCacheCleanArgs {
    /// Remove every package cache entry instead of only entries unused by the current harn.lock.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageCacheVerifyArgs {
    /// Also verify materialized packages under .harn/packages/.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub materialized: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelInfoArgs {
    /// Verify provider-local readiness for the resolved model when supported.
    #[arg(long)]
    pub verify: bool,
    /// Warm/preload the resolved model when supported. Implies --verify.
    #[arg(long)]
    pub warm: bool,
    /// Ollama keep_alive value to use with --warm (for example 30m, forever, or -1).
    #[arg(long = "keep-alive", value_name = "VALUE")]
    pub keep_alive: Option<String>,
    /// Model alias or provider-native model id.
    pub model: String,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderCatalogArgs {
    /// Only include providers that are usable in the current environment.
    #[arg(long)]
    pub available_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderReadyArgs {
    /// Provider id from Harn provider config, for example mlx or local.
    pub provider: String,
    /// Model alias or provider-native model id to require in /models.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the configured provider base URL for this probe.
    #[arg(long = "base-url")]
    pub base_url: Option<String>,
    /// Emit the full structured readiness result as JSON.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
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
        Cli, Command, ConnectCommand, McpCommand, OrchestratorCommand, OrchestratorDeployProvider,
        OrchestratorLogFormat, OrchestratorQueueCommand, OrchestratorTenantCommand,
        PackageCacheCommand, PackageCommand, ProjectTemplate, RunsCommand, SkillCommand,
        SkillKeyCommand, SkillTrustCommand, SkillsCommand, TraceCommand, TriggerCommand,
        TrustCommand, TrustOutcomeArg, TrustTierArg,
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
    fn test_parses_agents_conformance_target_url() {
        let cli = Cli::parse_from([
            "harn",
            "test",
            "agents-conformance",
            "--target",
            "http://localhost:8080",
            "--api-key",
            "test-key",
            "--category",
            "core,streaming",
            "--json",
        ]);

        let Command::Test(args) = cli.command.unwrap() else {
            panic!("expected test command");
        };
        assert_eq!(args.target.as_deref(), Some("agents-conformance"));
        assert_eq!(args.agents_target.as_deref(), Some("http://localhost:8080"));
        assert_eq!(args.agents_api_key.as_deref(), Some("test-key"));
        assert_eq!(args.agents_category, vec!["core,streaming"]);
        assert!(args.json);
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
    fn test_parses_connect_oauth_flags() {
        let cli = Cli::parse_from([
            "harn",
            "connect",
            "slack",
            "--client-id",
            "client",
            "--client-secret",
            "secret",
            "--scope",
            "chat:write app_mentions:read",
            "--no-open",
        ]);

        let Command::Connect(args) = cli.command.unwrap() else {
            panic!("expected connect command");
        };
        let Some(ConnectCommand::Slack(slack)) = args.command else {
            panic!("expected connect slack");
        };
        assert_eq!(slack.client_id.as_deref(), Some("client"));
        assert_eq!(slack.client_secret.as_deref(), Some("secret"));
        assert_eq!(slack.scope.as_deref(), Some("chat:write app_mentions:read"));
        assert!(slack.no_open);

        let cli = Cli::parse_from([
            "harn",
            "connect",
            "linear",
            "--client-id",
            "linear-client",
            "--client-secret",
            "linear-secret",
        ]);
        let Command::Connect(args) = cli.command.unwrap() else {
            panic!("expected connect command");
        };
        let Some(ConnectCommand::Linear(linear)) = args.command else {
            panic!("expected connect linear");
        };
        assert!(linear.url.is_none());
        assert_eq!(linear.client_id.as_deref(), Some("linear-client"));

        let cli = Cli::parse_from([
            "harn",
            "connect",
            "acme",
            "--client-id",
            "acme-client",
            "--scope",
            "tickets.read",
            "--no-open",
        ]);
        let Command::Connect(args) = cli.command.unwrap() else {
            panic!("expected connect command");
        };
        let Some(ConnectCommand::Provider(raw)) = args.command else {
            panic!("expected external provider connect command");
        };
        assert_eq!(
            raw,
            vec![
                "acme".to_string(),
                "--client-id".to_string(),
                "acme-client".to_string(),
                "--scope".to_string(),
                "tickets.read".to_string(),
                "--no-open".to_string()
            ]
        );
    }

    #[test]
    fn test_parses_connect_management_flags() {
        let cli = Cli::parse_from(["harn", "connect", "--list", "--json"]);

        let Command::Connect(args) = cli.command.unwrap() else {
            panic!("expected connect command");
        };
        assert!(args.list);
        assert!(args.json);
        assert!(args.command.is_none());

        let cli = Cli::parse_from([
            "harn",
            "connect",
            "--generic",
            "acme",
            "https://mcp.example.com/mcp",
        ]);
        let Command::Connect(args) = cli.command.unwrap() else {
            panic!("expected connect command");
        };
        assert_eq!(args.generic, vec!["acme", "https://mcp.example.com/mcp"]);
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
    fn test_parses_serve_mcp_flags() {
        let cli = Cli::parse_from([
            "harn",
            "serve",
            "mcp",
            "--transport",
            "http",
            "--bind",
            "127.0.0.1:9001",
            "--path",
            "/rpc",
            "--sse-path",
            "/events",
            "--messages-path",
            "/legacy/messages",
            "--api-key",
            "alpha,beta",
            "--hmac-secret",
            "shared",
            "--tls",
            "pem",
            "--cert",
            "tls/cert.pem",
            "--key",
            "tls/key.pem",
            "server.harn",
        ]);

        let Command::Serve(args) = cli.command.unwrap() else {
            panic!("expected serve command");
        };
        let crate::cli::ServeCommand::Mcp(serve) = args.command else {
            panic!("expected serve mcp");
        };
        assert_eq!(serve.transport, crate::cli::McpServeTransport::Http);
        assert_eq!(serve.bind.to_string(), "127.0.0.1:9001");
        assert_eq!(serve.path, "/rpc");
        assert_eq!(serve.sse_path, "/events");
        assert_eq!(serve.messages_path, "/legacy/messages");
        assert_eq!(serve.api_key, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(serve.hmac_secret.as_deref(), Some("shared"));
        assert_eq!(serve.tls, crate::cli::ServeTlsMode::Pem);
        assert_eq!(serve.cert, Some(PathBuf::from("tls/cert.pem")));
        assert_eq!(serve.key, Some(PathBuf::from("tls/key.pem")));
        assert_eq!(serve.file, "server.harn");
    }

    #[test]
    fn test_parses_serve_acp() {
        let cli = Cli::parse_from(["harn", "serve", "acp", "agent.harn"]);

        let Command::Serve(args) = cli.command.unwrap() else {
            panic!("expected serve command");
        };
        let crate::cli::ServeCommand::Acp(serve) = args.command else {
            panic!("expected serve acp");
        };
        assert_eq!(serve.file, "agent.harn");
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
    fn test_parses_trace_import_args() {
        let cli = Cli::parse_from([
            "harn",
            "trace",
            "import",
            "--trace-file",
            "langfuse.jsonl",
            "--trace-id",
            "trace_123",
            "--output",
            "fixtures/imported.jsonl",
        ]);

        let Command::Trace(args) = cli.command.unwrap() else {
            panic!("expected trace command");
        };
        let TraceCommand::Import(import) = args.command;
        assert_eq!(import.trace_file, "langfuse.jsonl");
        assert_eq!(import.trace_id.as_deref(), Some("trace_123"));
        assert_eq!(import.output, "fixtures/imported.jsonl");
    }

    #[test]
    fn test_parses_crystallize_args() {
        let cli = Cli::parse_from([
            "harn",
            "crystallize",
            "--from",
            "fixtures/crystallize",
            "--out",
            "workflows/version_bump.harn",
            "--report",
            "reports/version_bump.json",
            "--eval-pack",
            "harn.eval.toml",
            "--min-examples",
            "5",
            "--workflow-name",
            "version_bump",
        ]);

        let Command::Crystallize(args) = cli.command.unwrap() else {
            panic!("expected crystallize command");
        };
        assert_eq!(args.from, "fixtures/crystallize");
        assert_eq!(args.out, "workflows/version_bump.harn");
        assert_eq!(args.report, "reports/version_bump.json");
        assert_eq!(args.eval_pack.as_deref(), Some("harn.eval.toml"));
        assert_eq!(args.min_examples, 5);
        assert_eq!(args.workflow_name.as_deref(), Some("version_bump"));
    }

    #[test]
    fn test_parses_package_evals_flag() {
        let cli = Cli::parse_from(["harn", "test", "package", "--evals"]);

        let Command::Test(args) = cli.command.unwrap() else {
            panic!("expected test command");
        };
        assert_eq!(args.target.as_deref(), Some("package"));
        assert!(args.evals);
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
    fn test_parses_test_determinism_flag() {
        let cli = Cli::parse_from([
            "harn",
            "test",
            "--determinism",
            "--filter",
            "agent",
            "tests/agent_loop.harn",
        ]);

        let Command::Test(args) = cli.command.unwrap() else {
            panic!("expected test command");
        };
        assert!(args.determinism);
        assert_eq!(args.filter.as_deref(), Some("agent"));
        assert_eq!(args.target.as_deref(), Some("tests/agent_loop.harn"));
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
            "--limit",
            "500",
            "--grouped-by-trace",
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
        assert_eq!(query.limit, Some(500));
        assert!(query.grouped_by_trace);
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
    fn test_parses_trust_graph_verify_chain() {
        let cli = Cli::parse_from(["harn", "trust-graph", "verify-chain", "--json"]);

        let Command::TrustGraph(args) = cli.command.unwrap() else {
            panic!("expected trust-graph command");
        };
        let TrustCommand::VerifyChain(verify) = args.command else {
            panic!("expected trust-graph verify-chain");
        };
        assert!(verify.json);
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
            "--pump-max-outstanding",
            "4",
            "--log-format",
            "json",
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
        assert_eq!(serve.pump_max_outstanding, Some(4));
        assert_eq!(serve.log_format, OrchestratorLogFormat::Json);
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
    fn test_parses_orchestrator_deploy_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "deploy",
            "--provider",
            "fly",
            "--manifest",
            "workspace/harn.toml",
            "--name",
            "harn-prod",
            "--image",
            "ghcr.io/acme/harn-prod:latest",
            "--deploy-dir",
            "ops/deploy",
            "--port",
            "8443",
            "--data-dir",
            "/data",
            "--disk-size-gb",
            "20",
            "--shutdown-timeout",
            "60",
            "--region",
            "sjc",
            "--build",
            "--env",
            "RUST_LOG=debug",
            "--secret",
            "OPENAI_API_KEY=sk-test",
            "--dry-run",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Deploy(deploy) = args.command else {
            panic!("expected orchestrator deploy");
        };
        assert_eq!(deploy.provider, OrchestratorDeployProvider::Fly);
        assert_eq!(deploy.manifest, PathBuf::from("workspace/harn.toml"));
        assert_eq!(deploy.name, "harn-prod");
        assert_eq!(deploy.image, "ghcr.io/acme/harn-prod:latest");
        assert_eq!(deploy.deploy_dir, PathBuf::from("ops/deploy"));
        assert_eq!(deploy.port, 8443);
        assert_eq!(deploy.data_dir, "/data");
        assert_eq!(deploy.disk_size_gb, 20);
        assert_eq!(deploy.shutdown_timeout, 60);
        assert_eq!(deploy.region.as_deref(), Some("sjc"));
        assert!(deploy.build);
        assert_eq!(deploy.env, vec!["RUST_LOG=debug".to_string()]);
        assert_eq!(deploy.secret, vec!["OPENAI_API_KEY=sk-test".to_string()]);
        assert!(deploy.dry_run);
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
    fn test_parses_orchestrator_tenant_create_args() {
        let cli = Cli::parse_from([
            "harn",
            "orchestrator",
            "tenant",
            "--state-dir",
            "state/orchestrator",
            "create",
            "acme",
            "--daily-cost-usd",
            "25.5",
            "--ingest-per-minute",
            "120",
            "--json",
        ]);

        let Command::Orchestrator(args) = cli.command.unwrap() else {
            panic!("expected orchestrator command");
        };
        let OrchestratorCommand::Tenant(tenant) = args.command else {
            panic!("expected orchestrator tenant");
        };
        let OrchestratorTenantCommand::Create(create) = tenant.command else {
            panic!("expected orchestrator tenant create");
        };
        assert_eq!(tenant.local.state_dir, PathBuf::from("state/orchestrator"));
        assert_eq!(create.id, "acme");
        assert_eq!(create.daily_cost_usd, Some(25.5));
        assert_eq!(create.ingest_per_minute, Some(120));
        assert!(create.json);
    }

    #[test]
    fn test_parses_new_template() {
        let cli = Cli::parse_from(["harn", "new", "review-bot", "--template", "agent"]);

        let Command::New(args) = cli.command.unwrap() else {
            panic!("expected new command");
        };
        assert_eq!(args.first.as_deref(), Some("review-bot"));
        assert_eq!(args.second.as_deref(), None);
        assert_eq!(args.template, Some(ProjectTemplate::Agent));
    }

    #[test]
    fn test_parses_new_package_kind() {
        let cli = Cli::parse_from(["harn", "new", "package", "acme-lib"]);

        let Command::New(args) = cli.command.unwrap() else {
            panic!("expected new command");
        };
        assert_eq!(args.first.as_deref(), Some("package"));
        assert_eq!(args.second.as_deref(), Some("acme-lib"));
        assert_eq!(args.template, None);
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
        assert_eq!(args.template, Some(ProjectTemplate::PipelineLab));
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
    fn test_parses_install_integrity_flags() {
        let cli = Cli::parse_from(["harn", "install", "--locked", "--offline"]);

        let Command::Install(args) = cli.command.unwrap() else {
            panic!("expected install command");
        };
        assert!(!args.frozen);
        assert!(args.locked);
        assert!(args.offline);
    }

    #[test]
    fn test_parses_add_registry_override() {
        let cli = Cli::parse_from([
            "harn",
            "add",
            "@burin/notion-sdk@1.2.3",
            "--registry",
            "index.toml",
        ]);

        let Command::Add(args) = cli.command.unwrap() else {
            panic!("expected add command");
        };
        assert_eq!(args.name_or_spec, "@burin/notion-sdk@1.2.3");
        assert_eq!(args.registry.as_deref(), Some("index.toml"));
    }

    #[test]
    fn test_parses_package_cache_subcommands() {
        let cli = Cli::parse_from([
            "harn",
            "package",
            "search",
            "notion",
            "--registry",
            "index.toml",
            "--json",
        ]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Search(search) = args.command else {
            panic!("expected package search");
        };
        assert_eq!(search.query.as_deref(), Some("notion"));
        assert_eq!(search.registry.as_deref(), Some("index.toml"));
        assert!(search.json);

        let cli = Cli::parse_from([
            "harn",
            "package",
            "info",
            "@burin/notion-sdk@1.2.3",
            "--registry",
            "index.toml",
        ]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Info(info) = args.command else {
            panic!("expected package info");
        };
        assert_eq!(info.name, "@burin/notion-sdk@1.2.3");
        assert_eq!(info.registry.as_deref(), Some("index.toml"));

        let cli = Cli::parse_from(["harn", "package", "check", "pkg", "--json"]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Check(check) = args.command else {
            panic!("expected package check");
        };
        assert_eq!(check.package, Some(PathBuf::from("pkg")));
        assert!(check.json);

        let cli = Cli::parse_from(["harn", "package", "pack", "pkg", "--dry-run"]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Pack(pack) = args.command else {
            panic!("expected package pack");
        };
        assert_eq!(pack.package, Some(PathBuf::from("pkg")));
        assert!(pack.dry_run);

        let cli = Cli::parse_from(["harn", "package", "docs", "pkg", "--check"]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Docs(docs) = args.command else {
            panic!("expected package docs");
        };
        assert_eq!(docs.package, Some(PathBuf::from("pkg")));
        assert!(docs.check);

        let cli = Cli::parse_from(["harn", "package", "cache", "list"]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Cache(cache) = args.command else {
            panic!("expected package cache");
        };
        assert!(matches!(cache.command, PackageCacheCommand::List));

        let cli = Cli::parse_from(["harn", "package", "cache", "clean", "--all"]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Cache(cache) = args.command else {
            panic!("expected package cache");
        };
        let PackageCacheCommand::Clean(clean) = cache.command else {
            panic!("expected package cache clean");
        };
        assert!(clean.all);

        let cli = Cli::parse_from(["harn", "package", "cache", "verify", "--materialized"]);
        let Command::Package(args) = cli.command.unwrap() else {
            panic!("expected package command");
        };
        let PackageCommand::Cache(cache) = args.command else {
            panic!("expected package cache");
        };
        let PackageCacheCommand::Verify(verify) = cache.command else {
            panic!("expected package cache verify");
        };
        assert!(verify.materialized);
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
        let cli = Cli::parse_from([
            "harn",
            "model-info",
            "--verify",
            "--warm",
            "--keep-alive",
            "forever",
            "tog-gemma4-31b",
        ]);

        let Command::ModelInfo(args) = cli.command.unwrap() else {
            panic!("expected model-info command");
        };
        assert_eq!(args.model, "tog-gemma4-31b");
        assert!(args.verify);
        assert!(args.warm);
        assert_eq!(args.keep_alive.as_deref(), Some("forever"));
    }

    #[test]
    fn test_parses_provider_catalog_args() {
        let cli = Cli::parse_from(["harn", "provider-catalog", "--available-only"]);

        let Command::ProviderCatalog(args) = cli.command.unwrap() else {
            panic!("expected provider-catalog command");
        };
        assert!(args.available_only);
    }

    #[test]
    fn test_parses_provider_ready_args() {
        let cli = Cli::parse_from([
            "harn",
            "provider-ready",
            "mlx",
            "--model",
            "mlx-qwen36-27b",
            "--base-url",
            "http://127.0.0.1:8002",
            "--json",
        ]);

        let Command::ProviderReady(args) = cli.command.unwrap() else {
            panic!("expected provider-ready command");
        };
        assert_eq!(args.provider, "mlx");
        assert_eq!(args.model.as_deref(), Some("mlx-qwen36-27b"));
        assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:8002"));
        assert!(args.json);
    }
}
