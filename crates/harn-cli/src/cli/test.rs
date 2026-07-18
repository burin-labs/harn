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
    /// Emit structured JSON for conformance or agents-conformance runs.
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,
    /// Write a user-test or agents-conformance JSON report to this path.
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
    /// Per-test timeout in milliseconds. For user suites, this bounds only
    /// pipeline execution; other targets bound their test case or subprocess.
    #[arg(long, default_value_t = 30_000)]
    pub timeout: u64,
    /// Fail a passing user test whose total wall-clock time exceeds this
    /// many milliseconds. Also honored via HARN_TEST_MAX_MS.
    #[arg(long = "max-test-ms", value_name = "MS", env = "HARN_TEST_MAX_MS")]
    pub max_test_ms: Option<u64>,
    /// Fail a passing user test whose test-body execution phase exceeds this
    /// many milliseconds. Also honored via HARN_TEST_MAX_EXECUTE_MS.
    #[arg(
        long = "max-execute-ms",
        value_name = "MS",
        env = "HARN_TEST_MAX_EXECUTE_MS"
    )]
    pub max_execute_ms: Option<u64>,
    /// Run user tests concurrently where supported.
    #[arg(long)]
    pub parallel: bool,
    /// Stop scheduling new user tests after the first failure. Tests already
    /// running under --parallel finish and remain in the report.
    #[arg(long)]
    pub fail_fast: bool,
    /// Maximum number of concurrent test workers. Defaults to available
    /// parallelism, capped both by core count and by currently-available
    /// system memory (so an auto-sized run backs off on a small or already-
    /// loaded host instead of overcommitting RAM). Tune the per-worker memory
    /// budget with `HARN_TEST_WORKER_MEMORY_MB`. This flag (also honored via
    /// the `HARN_TEST_JOBS` env var) overrides both caps. Ignored unless
    /// `--parallel` is set.
    #[arg(long = "jobs", short = 'j', value_name = "N", env = "HARN_TEST_JOBS")]
    pub jobs: Option<usize>,
    /// 1-based shard index for user or conformance tests. Pair with `--shard-total`.
    #[arg(long = "shard-index", value_name = "N", env = "HARN_TEST_SHARD_INDEX")]
    pub shard_index: Option<usize>,
    /// Total number of user or conformance test shards. Pair with `--shard-index`.
    #[arg(long = "shard-total", value_name = "N", env = "HARN_TEST_SHARD_TOTAL")]
    pub shard_total: Option<usize>,
    /// Re-run user tests when watched files change.
    #[arg(long)]
    pub watch: bool,
    /// Collect line coverage for executed Harn source and print a per-file
    /// summary after the run. Supported for user test suites.
    #[arg(long, action = ArgAction::SetTrue)]
    pub coverage: bool,
    /// Write an LCOV tracefile to this path (implies --coverage). Consumable by
    /// Codecov, `genhtml`, and the VS Code Coverage Gutters extension.
    #[arg(long = "coverage-out", value_name = "PATH")]
    pub coverage_out: Option<String>,
    /// Show per-test timing and detailed failures. Also prints a passing
    /// case's `log`/`print`/`println` output, which is otherwise shown only
    /// for failing cases.
    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue)]
    pub verbose: bool,
    /// Show detailed timing for user and conformance suites.
    #[arg(long, action = ArgAction::SetTrue)]
    pub timing: bool,
    /// Emit per-test top-level and module-attribution phases to stderr.
    /// Also honored via `HARN_TEST_DIAGNOSE=1`.
    #[arg(long, action = ArgAction::SetTrue)]
    pub diagnose: bool,
    /// Record LLM fixtures to .harn-fixtures/.
    #[arg(long)]
    pub record: bool,
    /// Replay LLM fixtures from .harn-fixtures/.
    #[arg(long)]
    pub replay: bool,
    /// Record then replay each selected pipeline and assert deterministic output.
    #[arg(long)]
    pub determinism: bool,
    /// Run conformance fixtures once with optimizer passes enabled and once disabled.
    #[arg(long = "differential-optimizations")]
    pub differential_optimizations: bool,
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
