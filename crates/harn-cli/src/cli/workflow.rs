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
    /// Apply, validate, or preview an agent-authored workflow patch.
    #[command(subcommand)]
    Patch(WorkflowPatchCommand),
    /// List the safe Harn function tools an agent may call from the
    /// workflow-patch authoring loop.
    FunctionTools(WorkflowFunctionToolsArgs),
    /// Check whether running a target bundle would widen a parent
    /// capability ceiling.
    NestedCeiling(WorkflowNestedCeilingArgs),
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
    /// Emit Mermaid graph text for quick debugging.
    #[arg(long)]
    pub mermaid: bool,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPatchCommand {
    /// Apply, validate, and capability-check a patch without writing
    /// anything. Emits diagnostics, the structural diff, and the
    /// capability delta.
    Validate(WorkflowPatchValidateArgs),
    /// Apply a patch and write the resulting bundle JSON to disk.
    Apply(WorkflowPatchApplyArgs),
    /// Apply a patch in memory and emit the normalized preview of the
    /// resulting bundle (graph, mermaid, validation).
    Preview(WorkflowPatchPreviewArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPatchValidateArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Workflow patch JSON path.
    #[arg(long)]
    pub patch: String,
    /// Optional parent capability policy JSON path. When supplied, the
    /// patch is rejected if it asks for more than this ceiling.
    #[arg(long = "parent-ceiling", value_name = "PATH")]
    pub parent_ceiling: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPatchApplyArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Workflow patch JSON path.
    #[arg(long)]
    pub patch: String,
    /// Output bundle JSON path.
    #[arg(long)]
    pub out: String,
    /// Optional parent capability policy JSON path; mirrors `validate`.
    #[arg(long = "parent-ceiling", value_name = "PATH")]
    pub parent_ceiling: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPatchPreviewArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Workflow patch JSON path.
    #[arg(long)]
    pub patch: String,
    /// Emit Mermaid graph text instead of JSON.
    #[arg(long)]
    pub mermaid: bool,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowFunctionToolsArgs {
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowNestedCeilingArgs {
    /// Portable workflow bundle JSON path.
    #[arg(long)]
    pub bundle: String,
    /// Parent capability policy JSON path.
    #[arg(long)]
    pub parent: String,
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
