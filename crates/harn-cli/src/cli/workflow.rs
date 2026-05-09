use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct WorkflowArgs {
    #[command(subcommand)]
    pub command: WorkflowCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCommand {
    /// Validate a portable workflow bundle contract.
    Validate(WorkflowValidateArgs),
    /// Preview the normalized graph, triggers, policies, and requirements.
    Preview(WorkflowPreviewArgs),
    /// Materialize a deterministic local run receipt for a bundle.
    Run(WorkflowRunArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowValidateArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPreviewArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowRunArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Trigger id to attach to the deterministic local receipt.
    #[arg(long)]
    pub trigger_id: Option<String>,
    /// Event id to attach to the deterministic local receipt.
    #[arg(long)]
    pub event_id: Option<String>,
    /// Write the receipt JSON to this path as well as stdout.
    #[arg(long)]
    pub receipt_out: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}
