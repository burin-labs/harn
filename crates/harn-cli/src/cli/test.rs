use clap::{ArgAction, Args, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum InternalConformanceWorkerMode {
    ExecuteXfail,
    SkipXfail,
}

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
    /// pipeline execution; setup and shared import-graph compilation are
    /// measured separately. Other targets bound their test case or subprocess.
    #[arg(long, default_value_t = 30_000)]
    pub timeout: u64,
    /// Explicitly authorize a named risky operation for user-test execution.
    /// Repeatable. Names are exact (for example `git.push`).
    #[arg(long = "approve-risky", value_name = "OPERATION")]
    pub approve_risky: Vec<String>,
    /// Compile imported route modules as an explicitly trusted Rust
    /// host-dispatch graph. This is required only when testing sources whose
    /// production embedder owns privileged host wiring such as `host_call`.
    #[arg(long, action = ArgAction::SetTrue)]
    pub trusted_host_dispatch: bool,
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
    /// Run user tests concurrently, or conformance tests in isolated processes.
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
    /// Harn JSON test receipt used as the shard-cost and regression baseline.
    /// The receipt must carry the same --timing-environment identity.
    #[arg(long = "timing-baseline", value_name = "PATH")]
    pub timing_baseline: Option<String>,
    /// Stable identity for the environment that produced or enforces timing
    /// receipts (for example `github-linux-x64`).
    #[arg(
        long = "timing-environment",
        value_name = "NAME",
        env = "HARN_TEST_TIMING_ENVIRONMENT"
    )]
    pub timing_environment: Option<String>,
    /// Fail cases that grow beyond this percentage of their receipt baseline.
    #[arg(long = "max-cost-regression-percent", default_value_t = 25)]
    pub max_cost_regression_percent: u64,
    /// Internal transport for the process-isolated conformance worker.
    #[arg(long = "internal-conformance-worker", value_name = "MODE", hide = true)]
    pub internal_conformance_worker: Option<InternalConformanceWorkerMode>,
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
    /// Additional user-test file or directory. Repeat to run a curated suite
    /// in one compile-once scheduler invocation.
    #[arg(long = "test-path", value_name = "PATH")]
    pub test_paths: Vec<String>,
    /// Run only user-test files affected by changes since this Git ref.
    ///
    /// Harn follows its resolved module graph from changed modules through all
    /// transitive importers. It falls back to the complete requested suite when
    /// the diff contains a deletion, rename, non-Harn file, or a module outside
    /// the graph, so an uncertain impact plan never weakens coverage.
    #[arg(long = "affected-from", value_name = "GIT_REF")]
    pub affected_from: Option<String>,
    /// Print the versioned affected-test plan as JSON without running tests.
    /// Requires `--affected-from`.
    #[arg(long, action = ArgAction::SetTrue)]
    pub plan: bool,
}
