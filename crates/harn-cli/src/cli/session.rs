use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SessionCommand {
    /// List the agent sessions this workspace has persisted.
    List(SessionListArgs),
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
    /// Check or rewrite the repository's run/session-view compatibility fixtures.
    #[command(hide = true)]
    ViewFixtures(SessionViewFixturesArgs),
}

/// Sessions are the input to `harn runs --from-session`, so they need a way to
/// be discovered. Without this the session id can only be recovered by opening
/// `.harn/session-store.sqlite` by hand, which leaves the reporting surface as
/// unreachable as it was before it accepted sessions at all.
#[derive(Debug, Args)]
pub(crate) struct SessionListArgs {
    /// Workspace root holding `.harn/session-store.sqlite`. Defaults to the
    /// current directory.
    #[arg(long, value_name = "PATH")]
    pub session_root: Option<PathBuf>,
    /// Show at most this many sessions, newest first.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Emit the listing as JSON.
    #[arg(long)]
    pub json: bool,
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

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("mode")
        .required(true)
        .multiple(false)
        .args(["check", "write"])
))]
pub(crate) struct SessionViewFixturesArgs {
    /// Repository root containing spec/run-view-fixtures.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub repository_root: PathBuf,
    /// Fail when a checked-in snapshot differs from the production projection.
    #[arg(long, conflicts_with = "write")]
    pub check: bool,
    /// Rewrite checked-in snapshots from the production projection.
    #[arg(long, conflicts_with = "check")]
    pub write: bool,
}
