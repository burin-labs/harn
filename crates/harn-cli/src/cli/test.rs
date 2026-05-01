use clap::{ArgAction, Args};

#[derive(Debug, Args)]
pub(crate) struct TestArgs {
    /// Only run tests whose names or paths contain this pattern.
    #[arg(long)]
    pub filter: Option<String>,
    /// Agents Protocol Harness base URL when running `harn test agents-conformance`.
    #[arg(long = "target", value_name = "URL")]
    pub agents_target: Option<String>,
    /// Bearer API key for `harn test agents-conformance`.
    #[arg(long = "api-key", env = "HARN_AGENTS_CONFORMANCE_API_KEY")]
    pub agents_api_key: Option<String>,
    /// Restrict `harn test agents-conformance` to one category. Repeatable or comma-separated.
    #[arg(long = "category", value_name = "NAME")]
    pub agents_category: Vec<String>,
    /// Emit the agents conformance leaderboard-shaped JSON report to stdout.
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,
    /// Write the agents conformance leaderboard-shaped JSON report to this path.
    #[arg(long = "json-out", value_name = "PATH")]
    pub json_out: Option<String>,
    /// Existing workspace id to reuse for agents conformance probes.
    #[arg(long = "workspace-id", value_name = "ID")]
    pub agents_workspace_id: Option<String>,
    /// Existing session id to reuse for agents conformance probes.
    #[arg(long = "session-id", value_name = "ID")]
    pub agents_session_id: Option<String>,
    /// Write a JUnit XML report to this path.
    #[arg(long)]
    pub junit: Option<String>,
    /// Per-test timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub timeout: u64,
    /// Run user tests concurrently where supported.
    #[arg(long)]
    pub parallel: bool,
    /// Re-run user tests when watched files change.
    #[arg(long)]
    pub watch: bool,
    /// Show per-test timing and detailed failures.
    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue)]
    pub verbose: bool,
    /// Show per-test timing and summary statistics.
    #[arg(long, action = ArgAction::SetTrue)]
    pub timing: bool,
    /// Record LLM fixtures to .harn-fixtures/.
    #[arg(long)]
    pub record: bool,
    /// Replay LLM fixtures from .harn-fixtures/.
    #[arg(long)]
    pub replay: bool,
    /// Record then replay each selected pipeline and assert deterministic output.
    #[arg(long)]
    pub determinism: bool,
    /// Run eval packs declared by the nearest package manifest.
    #[arg(long)]
    pub evals: bool,
    /// Extra skill-discovery roots (repeatable). See `harn run
    /// --skill-dir` — applied the same way to user tests and
    /// conformance fixtures so bundled `skills/` dirs are picked up.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// User test path, `conformance`, or `protocols`.
    pub target: Option<String>,
    /// Optional file or directory under conformance/ or conformance/protocols/.
    pub selection: Option<String>,
}
