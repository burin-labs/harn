use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct CanonArgs {
    #[command(subcommand)]
    pub command: CanonCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CanonCommand {
    /// Evaluate harn-canon invariant packs against changed files.
    Check(CanonCheckArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CanonCheckArgs {
    /// File paths to evaluate. Relative paths are resolved under --workspace-root.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<PathBuf>,
    /// Directory containing canon-packs.json. Defaults to HARN_CANON_ROOT or workspace .harn/canon.
    #[arg(long = "canon-root", value_name = "PATH")]
    pub canon_root: Option<PathBuf>,
    /// Workspace root used to read relative PATH arguments.
    #[arg(
        long = "workspace-root",
        alias = "root",
        value_name = "PATH",
        default_value = "."
    )]
    pub workspace_root: PathBuf,
    /// Explicit harn-canon pack id. Repeat to bypass manifest path routing.
    #[arg(long = "pack", alias = "pack-id", value_name = "ID")]
    pub packs: Vec<String>,
    /// Include missing paths in the evaluated slice with empty text.
    #[arg(long = "include-missing")]
    pub include_missing: bool,
    /// Run semantic predicates as well as deterministic predicates.
    #[arg(long = "include-semantic")]
    pub include_semantic: bool,
    /// Per-pack predicate budget in milliseconds.
    #[arg(long = "budget-ms", value_name = "MS", default_value_t = 50)]
    pub budget_ms: u64,
    /// Print findings but exit zero even when harn-canon reports blocking findings.
    #[arg(long)]
    pub advisory: bool,
    /// Header used for human feedback text.
    #[arg(
        long = "feedback-header",
        value_name = "TEXT",
        default_value = "harn-canon feedback"
    )]
    pub feedback_header: String,
    /// Emit a stable JSON envelope.
    #[arg(long)]
    pub json: bool,
}
