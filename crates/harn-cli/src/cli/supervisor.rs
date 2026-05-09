use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use clap::{ArgAction, Args, Subcommand};

use super::orchestrator::OrchestratorLocalArgs;
use super::util::parse_duration_arg;

#[derive(Debug, Args)]
pub(crate) struct SupervisorArgs {
    #[command(subcommand)]
    pub command: SupervisorCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SupervisorCommand {
    /// Start a local workflow supervisor process.
    Start(SupervisorStartArgs),
    /// Stop the local workflow supervisor process.
    Stop(SupervisorStopArgs),
    /// List supervised workflow automations.
    List(SupervisorListArgs),
    /// Inspect one workflow automation or the full supervisor surface.
    Inspect(SupervisorInspectArgs),
    /// Pause a workflow automation until resumed.
    Pause(SupervisorPauseArgs),
    /// Resume a paused workflow automation.
    Resume(SupervisorResumeArgs),
    /// Fire a supervised workflow trigger.
    Fire(SupervisorFireArgs),
    /// Replay a supervised workflow event.
    Replay(SupervisorReplayArgs),
    /// Inspect or replay supervised workflow DLQ entries.
    Dlq(SupervisorDlqArgs),
    /// Recover stranded inflight events.
    Recover(SupervisorRecoverArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorStartArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Socket address the local supervisor should bind.
    #[arg(long, default_value = "127.0.0.1:8080", value_name = "ADDR")]
    pub bind: SocketAddr,
    /// Mount the orchestrator MCP server on the local listener.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub mcp: bool,
    /// Log file for the supervised process.
    #[arg(long = "log-file", value_name = "PATH")]
    pub log_file: Option<PathBuf>,
    /// Time to wait for the listener snapshot before returning.
    #[arg(long = "wait", value_name = "DURATION", value_parser = parse_duration_arg, default_value = "10s")]
    pub wait: StdDuration,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorStopArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorListArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorInspectArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Workflow/trigger id to inspect.
    pub workflow_id: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorPauseArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Workflow/trigger id to pause.
    pub workflow_id: String,
    /// Human-readable reason stored for host resume UX.
    #[arg(long)]
    pub reason: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorResumeArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Workflow/trigger id to resume.
    pub workflow_id: String,
    /// Human-readable reason stored for audit context.
    #[arg(long)]
    pub reason: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorFireArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Workflow/trigger id to fire.
    pub workflow_id: String,
    /// JSON object merged into the synthetic trigger event.
    #[arg(long = "payload-json", default_value = "{}", value_name = "JSON")]
    pub payload_json: String,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorReplayArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Existing event id to replay.
    pub event_id: String,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorDlqArgs {
    #[command(subcommand)]
    pub command: SupervisorDlqCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SupervisorDlqCommand {
    /// List pending DLQ entries.
    List(SupervisorDlqListArgs),
    /// Replay a pending DLQ entry.
    Replay(SupervisorDlqReplayArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorDlqListArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorDlqReplayArgs {
    #[command(flatten)]
    pub local: OrchestratorLocalArgs,
    /// DLQ entry id to replay.
    pub entry_id: String,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SupervisorRecoverArgs {
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
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}
