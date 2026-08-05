use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct FlowArgs {
    #[command(subcommand)]
    pub command: FlowCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum FlowCommand {
    /// Replay-audit historical slices against the current invariant predicates.
    ReplayAudit(FlowReplayAuditArgs),
    /// Ship Captain Phase 0 utilities.
    Ship(FlowShipArgs),
    /// Archivist Phase 0 utilities.
    Archivist(FlowArchivistArgs),
}

#[derive(Debug, Args)]
pub(crate) struct FlowReplayAuditArgs {
    /// SQLite Flow store path.
    #[arg(long, value_name = "PATH", default_value = ".harn/flow.sqlite")]
    pub store: PathBuf,
    /// Repo root used to discover current `invariants.harn` predicates.
    #[arg(
        long = "predicate-root",
        alias = "root",
        value_name = "PATH",
        default_value = "."
    )]
    pub predicate_root: PathBuf,
    /// Touched directory to resolve predicates for. Repeat for cross-directory slices.
    #[arg(long = "touched-dir", alias = "target-dir", value_name = "PATH")]
    pub touched_dirs: Vec<PathBuf>,
    /// SQLite created_at lower bound, for example `2026-04-26` or `2026-04-26 12:00:00`.
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Emit JSON instead of text.
    #[arg(long)]
    pub json: bool,
    /// Exit non-zero when drift is detected.
    #[arg(long)]
    pub fail_on_drift: bool,
}

#[derive(Debug, Args)]
pub(crate) struct FlowShipArgs {
    #[command(subcommand)]
    pub command: FlowShipCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum FlowShipCommand {
    /// Derive a candidate slice from stored atoms and emit a mock PR receipt.
    Watch(FlowShipWatchArgs),
}

#[derive(Debug, Args)]
pub(crate) struct FlowShipWatchArgs {
    /// SQLite Flow store path.
    #[arg(long, value_name = "PATH", default_value = ".harn/flow.sqlite")]
    pub store: PathBuf,
    /// Repo root used to discover current `invariants.harn` predicates.
    #[arg(
        long = "predicate-root",
        alias = "root",
        value_name = "PATH",
        default_value = "."
    )]
    pub predicate_root: PathBuf,
    /// Touched directory to resolve predicates for. Repeat for cross-directory slices.
    #[arg(long = "touched-dir", alias = "target-dir", value_name = "PATH")]
    pub touched_dirs: Vec<PathBuf>,
    /// Persona id written into the Phase 0 receipt.
    #[arg(long, value_name = "NAME", default_value = "ship_captain")]
    pub persona: String,
    /// Write the Phase 0 mock PR receipt to this path.
    #[arg(long = "mock-pr-out", value_name = "PATH")]
    pub mock_pr_out: Option<PathBuf>,
    /// Emit JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct FlowArchivistArgs {
    #[command(subcommand)]
    pub command: FlowArchivistCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum FlowArchivistCommand {
    /// Scan a repo and emit review-ready predicate proposal metadata.
    Scan(FlowArchivistScanArgs),
}

#[derive(Debug, Args)]
pub(crate) struct FlowArchivistScanArgs {
    /// Repo root to scan.
    #[arg(value_name = "REPO", default_value = ".")]
    pub repo: PathBuf,
    /// Optional persona manifest to validate. Defaults to repo-local Flow persona manifests when present.
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// SQLite Flow store path used for shadow evaluation.
    #[arg(long, value_name = "PATH", default_value = ".harn/flow.sqlite")]
    pub store: PathBuf,
    /// Number of recent atom days to include in shadow evaluation.
    #[arg(long = "shadow-days", value_name = "DAYS", default_value_t = 30)]
    pub shadow_days: u32,
    /// Write the proposal JSON to this path.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Emit JSON instead of text.
    #[arg(long)]
    pub json: bool,
}
