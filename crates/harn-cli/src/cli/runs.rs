use clap::{Args, Subcommand};

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
    /// Path to the run record JSON file. Kept for compatibility with older `harn replay <path>` usage.
    #[arg(
        value_name = "PATH",
        required_unless_present_any = ["fixture", "session_id"],
        conflicts_with_all = ["fixture", "session_id"]
    )]
    pub path: Option<String>,
    /// Path to a run record or replay-oracle fixture.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["path", "session_id"])]
    pub fixture: Option<String>,
    /// Reconstruct replay input from the agent-session events in `--events-db`.
    #[arg(long, requires = "events_db", conflicts_with_all = ["path", "fixture"])]
    pub session_id: Option<String>,
    /// SQLite EventLog database to read for `--session-id`.
    #[arg(long, value_name = "PATH", requires = "session_id")]
    pub events_db: Option<String>,
    /// Time-travel: rehydrate the session only up to (and including) this
    /// event id, replaying it as it stood at that point. Requires
    /// `--session-id`; omit to replay the whole session.
    #[arg(long, value_name = "EVENT_ID", requires = "session_id")]
    pub at: Option<u64>,
    /// Counterfactual: after rehydrating the session at `--at` (or its full
    /// state), evaluate one or more `.harn` plans and report how the workspace
    /// *would have* diverged — the set of files the chained edits would
    /// touch. The plans run through `edit.dry_run` against a throw-away
    /// staged-fs overlay (#1722), so the recorded session and the on-disk
    /// tree are never mutated. Requires `--session-id`.
    #[arg(long, value_name = "PLAN", requires = "session_id")]
    pub counterfactual: Vec<String>,
    /// Number of replay reads to compare for deterministic output.
    #[arg(long, default_value_t = 1)]
    pub runs: usize,
    /// Emit a structured `JsonEnvelope` replay summary instead of human-readable output.
    /// See `docs/src/cli-json-contract.md` for the envelope shape.
    #[arg(long)]
    pub json: bool,
}
