use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use clap::{ArgAction, Args, Subcommand, ValueEnum};

use super::util::parse_duration_arg;

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
    Deploy(Box<OrchestratorDeployArgs>),
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
    /// Run canonical replay determinism oracle fixtures.
    ReplayOracle(OrchestratorReplayOracleArgs),
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
    /// Mount the orchestrator MCP HTTP server on this listener.
    #[arg(long = "mcp", default_value_t = false, action = ArgAction::SetTrue)]
    pub mcp: bool,
    /// Streamable HTTP endpoint path for the embedded MCP server.
    #[arg(long = "mcp-path", default_value = "/mcp", value_name = "PATH")]
    pub mcp_path: String,
    /// Legacy SSE endpoint path for older MCP clients.
    #[arg(long = "mcp-sse-path", default_value = "/sse", value_name = "PATH")]
    pub mcp_sse_path: String,
    /// Legacy SSE POST endpoint path for older MCP clients.
    #[arg(
        long = "mcp-messages-path",
        default_value = "/messages",
        value_name = "PATH"
    )]
    pub mcp_messages_path: String,
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
    /// Expose the Prometheus `/metrics` endpoint unauthenticated.
    /// When false (default), `/metrics` is gated behind the same
    /// `ListenerAuth` as ingestion so a public bind does not leak the
    /// route catalog, trigger names, and throughput counters that map
    /// the workflow topology to anyone who can reach the listener.
    #[arg(
        long = "public-metrics",
        env = "HARN_ORCHESTRATOR_PUBLIC_METRICS",
        default_value_t = false,
        action = ArgAction::SetTrue
    )]
    pub public_metrics: bool,
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
    /// Railway project id for API-backed secret synchronization.
    #[arg(
        long = "railway-project",
        env = "RAILWAY_PROJECT_ID",
        value_name = "PROJECT_ID"
    )]
    pub railway_project: Option<String>,
    /// Render API key for API-backed secret synchronization.
    #[arg(long = "render-api-key", env = "RENDER_API_KEY", value_name = "TOKEN")]
    pub render_api_key: Option<String>,
    /// Fly API token for API-backed secret synchronization.
    #[arg(long = "fly-api-token", env = "FLY_API_TOKEN", value_name = "TOKEN")]
    pub fly_api_token: Option<String>,
    /// Railway API token for API-backed secret synchronization.
    #[arg(long = "railway-token", env = "RAILWAY_TOKEN", value_name = "TOKEN")]
    pub railway_token: Option<String>,
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
pub(crate) struct OrchestratorReplayOracleArgs {
    /// Fixture file or directory to run. Defaults to the checked-in replay oracle fixtures.
    #[arg(value_name = "PATH")]
    pub selection: Option<PathBuf>,
    /// Run only fixtures whose name, path, or protocol fixture refs contain this substring.
    #[arg(long, value_name = "TEXT")]
    pub filter: Option<String>,
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
