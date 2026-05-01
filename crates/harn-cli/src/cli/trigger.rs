use clap::{Args, Subcommand};

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
