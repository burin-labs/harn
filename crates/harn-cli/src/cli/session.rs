use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SessionCommand {
    /// Export a persisted run record as a portable Harn session bundle.
    Export(SessionExportArgs),
    /// Export a suspended worker snapshot as a resumable checkpoint bundle.
    Checkpoint(SessionCheckpointArgs),
    /// Import a Harn session bundle back into a local run record.
    Import(SessionImportArgs),
    /// Validate a Harn session bundle without importing it.
    Validate(SessionValidateArgs),
    /// Print or check the generated session-bundle JSON Schema.
    Schema(SessionSchemaArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SessionExportArgs {
    /// Path to the run record JSON file to export.
    pub run_record: String,
    /// Write the bundle to this path. Prints JSON to stdout when omitted.
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
    /// Preserve local-only content instead of applying the default redaction policy.
    #[arg(long, conflicts_with = "replay_only")]
    pub local: bool,
    /// Export replay metadata with prompt/tool payloads withheld.
    #[arg(long, conflicts_with = "local")]
    pub replay_only: bool,
    /// Include artifact payloads in the bundle. Omitted by default for share safety.
    #[arg(long)]
    pub include_attachments: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SessionCheckpointArgs {
    /// Path to a suspended worker snapshot JSON file.
    pub worker_snapshot: String,
    /// Write the checkpoint bundle to this path. Prints JSON to stdout when omitted.
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
    /// Redact local-only content for share-safe inspection. The default local mode preserves resumability.
    #[arg(long, conflicts_with = "replay_only")]
    pub sanitized: bool,
    /// Export replay metadata with prompt/tool payloads withheld.
    #[arg(long, conflicts_with = "sanitized")]
    pub replay_only: bool,
    /// Include artifact payloads in the bundle. Omitted by default for share safety.
    #[arg(long)]
    pub include_attachments: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SessionImportArgs {
    /// Path to a Harn session bundle JSON file.
    pub bundle: String,
    /// Write the imported run record to this path.
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
    /// Directory for worker snapshots embedded in the bundle.
    #[arg(long, value_name = "PATH")]
    pub worker_snapshot_dir: Option<String>,
    /// Allow bundles that still contain high-confidence secret markers.
    #[arg(long)]
    pub allow_unsafe_secret_markers: bool,
    /// Print a JSON import report with materialized worker snapshot resume commands.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SessionValidateArgs {
    /// Path to a Harn session bundle JSON file.
    pub bundle: String,
    /// Allow bundles that still contain high-confidence secret markers.
    #[arg(long)]
    pub allow_unsafe_secret_markers: bool,
    /// Print a compact JSON summary on success.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SessionSchemaArgs {
    /// Check that the checked-in schema file is up to date.
    #[arg(long)]
    pub check: bool,
    /// Schema path used by --check, or write destination when --check is absent.
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
}
