use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::env_guard::ScopedEnvVar;
use crate::package;
use crate::test_timing::DurationSummary;
use crate::CLI_RUNTIME_STACK_SIZE;
use harn_vm::IsolateValue;

mod execution;
#[cfg(test)]
mod fixture_tests;
mod skill_context;
#[cfg(test)]
mod tests;

use execution::{execute_case, execute_file_fixture};
use harn_test_runner::{
    extract_cases_from_program, parse_program, prepare_callable_entries,
    seed_imported_enum_candidates, FixtureScope, TestCase, TestFixture,
};
pub use harn_test_runner::{
    AggregateTimings, PhaseTimings, SuiteCallablePreparation, TestPhase, TestResult, TestSummary,
    TestTimeout,
};
pub use harn_test_runner::{TestRunSession, TestRunSessionStats};
use skill_context::PreparedSkillContexts;

pub use harn_test_runner::{TestRunEvent, TestRunProgress};

const LARGE_SEQUENTIAL_TEST_THRESHOLD: usize = 50;
const LARGE_SEQUENTIAL_FILE_THRESHOLD: usize = 10;
const DEFAULT_PARALLEL_JOBS_CAP: usize = 8;
const TIMINGS_CACHE_RELATIVE_PATH: &str = ".harn/test-timings.json";
const HARN_TEST_JOBS_ENV: &str = "HARN_TEST_JOBS";
const HARN_TEST_MAX_MS_ENV: &str = "HARN_TEST_MAX_MS";
const HARN_TEST_MAX_EXECUTE_MS_ENV: &str = "HARN_TEST_MAX_EXECUTE_MS";

/// Per-worker memory budget (MiB) used to cap *auto-detected* parallelism on
/// memory-constrained or oversubscribed hosts. Overridable via
/// `HARN_TEST_WORKER_MEMORY_MB`. A worker runs a full VM and may drive nested
/// agent loops, so this is a deliberately conservative estimate. The cap only
/// ever *lowers* the core-based default — it never raises it, and an explicit
/// `--jobs` / `HARN_TEST_JOBS` always wins.
const DEFAULT_WORKER_MEMORY_MB: u64 = 1024;
const HARN_TEST_WORKER_MEMORY_MB_ENV: &str = "HARN_TEST_WORKER_MEMORY_MB";

/// Memory (MiB) held back for the OS, the CI runner agent, and any co-tenant
/// job, so an auto-sized suite cannot consume the last scrap of RAM and starve
/// the runner's heartbeat. This is the failure mode behind the self-hosted
/// "The operation was canceled" runner-loss cancellations: two runner agents
/// share one box, two heavy jobs overcommit RAM + swap, and the kernel never
/// fires the OOM-killer — instead a starved runner agent stops phoning home
/// and the control plane declares the job lost.
const RESERVED_SYSTEM_MEMORY_MB: u64 = 1024;

/// Options that shape how a user-test suite is discovered and executed.
///
/// Held separately from the positional path so call sites (one-shot run,
/// `--watch`, persona doctor) can share the same scheduler without
/// keyword-argument explosion at the call sites.
#[derive(Clone, Default)]
pub struct RunOptions {
    pub filter: Option<String>,
    pub timeout_ms: u64,
    /// Optional hard budget for a passing test's total wall-clock duration.
    /// Exceeding it converts the result to a failure without changing the
    /// actual per-test timeout behavior.
    pub max_test_ms: Option<u64>,
    /// Optional hard budget for a passing test's `vm.execute` phase. This
    /// catches tests whose assertions accidentally drive full agent loops or
    /// other slow runtime behavior while ignoring setup/compile cold-start.
    pub max_execute_ms: Option<u64>,
    /// When false, the scheduler runs with a single worker, preserving the
    /// historical "everything sequential" semantics that `harn test`
    /// defaulted to before `--parallel` was introduced.
    pub parallel: bool,
    /// Stop claiming new cases after the first discovery or execution failure.
    /// Cases already running in parallel finish and retain their results.
    pub fail_fast: bool,
    /// Explicit worker limit (`-j`/`--jobs`). `None` defaults to the
    /// available parallelism, capped by a small constant when running in
    /// parallel mode. Ignored when `parallel = false`.
    pub jobs: Option<usize>,
    /// Optional 1-based shard selection for CI matrix fan-out. Sharding
    /// happens after discovery/filtering and before execution.
    pub shard: Option<TestShard>,
    pub cli_skill_dirs: Vec<PathBuf>,
    /// Optional progress callback. When set, the runner emits events as
    /// the suite progresses; consumers (CLI, dev mode) render output.
    pub progress: Option<TestRunProgress>,
    /// Emit per-test phase timings (setup / compile / execute /
    /// teardown) to stderr. Also honored via `HARN_TEST_DIAGNOSE=1` so
    /// users can flip the flag without restarting their shell.
    pub diagnose: bool,
    /// Run each case in an explicitly trusted host-dispatch VM. This keeps
    /// privileged wire access behind an operator-selected test boundary.
    pub trusted_host_dispatch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestShard {
    index: usize,
    total: usize,
}

impl TestShard {
    pub fn new(index: usize, total: usize) -> Result<Self, String> {
        if total == 0 {
            return Err("test shard total must be at least 1".to_string());
        }
        if index == 0 {
            return Err("test shard index must be at least 1".to_string());
        }
        if index > total {
            return Err(format!(
                "test shard index {index} exceeds shard total {total}"
            ));
        }
        Ok(Self { index, total })
    }

    pub fn index(self) -> usize {
        self.index
    }

    pub fn total(self) -> usize {
        self.total
    }
}

impl RunOptions {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout_ms,
            ..Default::default()
        }
    }
}

fn canonicalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn test_execution_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn emit_progress(progress: &Option<TestRunProgress>, event: TestRunEvent) {
    if let Some(callback) = progress {
        callback(event);
    }
}

fn should_warn_large_sequential_suite(total_tests: usize, total_files: usize) -> bool {
    total_tests >= LARGE_SEQUENTIAL_TEST_THRESHOLD || total_files >= LARGE_SEQUENTIAL_FILE_THRESHOLD
}

/// Discover and run tests in a file or directory.
pub async fn run_tests(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
    cli_skill_dirs: &[PathBuf],
) -> TestSummary {
    let options = RunOptions {
        filter: filter.map(str::to_owned),
        timeout_ms,
        max_test_ms: test_budget_ms_via_env(HARN_TEST_MAX_MS_ENV),
        max_execute_ms: test_budget_ms_via_env(HARN_TEST_MAX_EXECUTE_MS_ENV),
        parallel,
        fail_fast: false,
        jobs: None,
        shard: None,
        cli_skill_dirs: cli_skill_dirs.to_vec(),
        progress: None,
        diagnose: diagnose_enabled_via_env(),
        trusted_host_dispatch: false,
    };
    run_tests_with_options(path, &options).await
}

/// Backwards-compatible progress-emitting entry point.
pub async fn run_tests_with_progress(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
    cli_skill_dirs: &[PathBuf],
    progress: Option<TestRunProgress>,
) -> TestSummary {
    let options = RunOptions {
        filter: filter.map(str::to_owned),
        timeout_ms,
        max_test_ms: test_budget_ms_via_env(HARN_TEST_MAX_MS_ENV),
        max_execute_ms: test_budget_ms_via_env(HARN_TEST_MAX_EXECUTE_MS_ENV),
        parallel,
        fail_fast: false,
        jobs: None,
        shard: None,
        cli_skill_dirs: cli_skill_dirs.to_vec(),
        progress,
        diagnose: diagnose_enabled_via_env(),
        trusted_host_dispatch: false,
    };
    run_tests_with_options(path, &options).await
}

fn diagnose_enabled_via_env() -> bool {
    let Ok(raw) = std::env::var("HARN_TEST_DIAGNOSE") else {
        return false;
    };
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn test_budget_ms_via_env(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&value| value >= 1)
}

/// Run tests with full control over scheduling, worker count, and
/// progress reporting. Workers and scheduling mode are reported via
/// `TestRunEvent::SuiteDiscovered` so consumers can render their own
/// banner instead of the runner printing to stdout directly.
pub async fn run_tests_with_options(path: &Path, options: &RunOptions) -> TestSummary {
    run_tests_with_session(path, options, &TestRunSession::default()).await
}

/// Run tests while retaining immutable prepared-module artifacts in `session`.
///
/// Callers that execute only once should use [`run_tests_with_options`]. Watch
/// mode and long-lived hosts should retain one session for their desired cache
/// lifetime and inspect [`TestRunSession::stats`] for reuse receipts.
pub fn run_tests_with_session<'a>(
    path: &'a Path,
    options: &'a RunOptions,
    session: &'a TestRunSession,
) -> Pin<Box<dyn Future<Output = TestSummary> + 'a>> {
    run_tests_with_session_and_operator_grant(path, options, session, None)
}

/// Run tests with an explicit operator grant that follows every worker.
///
/// `harn test --parallel` uses dedicated OS threads, so a thread-local grant
/// installed by the CLI must be transported through this runner explicitly.
pub(crate) fn run_tests_with_session_and_operator_grant<'a>(
    path: &'a Path,
    options: &'a RunOptions,
    session: &'a TestRunSession,
    operator_approval_grant: Option<&'a harn_vm::orchestration::OperatorApprovalGrant>,
) -> Pin<Box<dyn Future<Output = TestSummary> + 'a>> {
    Box::pin(async move {
        let paths = [path.to_path_buf()];
        run_tests_with_paths_and_operator_grant(&paths, options, session, operator_approval_grant)
            .await
    })
}

/// Run a curated set of files/directories through one discovery, import-graph
/// preparation, and scheduler session. Overlapping paths are deduplicated.
pub(crate) fn run_tests_with_paths_and_operator_grant<'a>(
    paths: &'a [PathBuf],
    options: &'a RunOptions,
    session: &'a TestRunSession,
    operator_approval_grant: Option<&'a harn_vm::orchestration::OperatorApprovalGrant>,
) -> Pin<Box<dyn Future<Output = TestSummary> + 'a>> {
    Box::pin(run_tests_with_session_impl(
        paths,
        options,
        session,
        operator_approval_grant,
    ))
}

async fn run_tests_with_session_impl(
    paths: &[PathBuf],
    options: &RunOptions,
    session: &TestRunSession,
    operator_approval_grant: Option<&harn_vm::orchestration::OperatorApprovalGrant>,
) -> TestSummary {
    // Default LLM provider to "mock" in test mode unless caller overrides.
    let _default_llm_provider = ScopedEnvVar::set_if_unset("HARN_LLM_PROVIDER", "mock");
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");

    let start = Instant::now();

    let collection_start = Instant::now();
    let canonical_targets = paths
        .iter()
        .map(|path| canonicalize_existing_path(path))
        .collect::<Vec<_>>();
    let mut files = canonical_targets
        .iter()
        .flat_map(|target| {
            if target.is_dir() {
                discover_test_files(target)
            } else {
                vec![target.clone()]
            }
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();

    let workers = resolve_workers(options);
    let timings_path = canonical_targets
        .first()
        .and_then(|target| timings_cache_path(target));
    let timings = timings_path
        .as_deref()
        .map(load_timings_cache)
        .unwrap_or_default();

    let mut discovery = discover_test_cases(&files, options.filter.as_deref(), workers);
    // `[check].trusted_host_dispatch` is the project's declaration that it is a
    // privileged embedder. `harn check` and `harn lint` both read it and OR the
    // CLI flag on top; `harn test` used to read only the flag, so a project
    // that declared the authority in its manifest still had every host_call
    // refused under test. That split made the manifest key mean one thing to
    // two commands and nothing to a third.
    let mut declared_dispatch: BTreeMap<PathBuf, bool> = BTreeMap::new();
    for case in &mut discovery.cases {
        let declared = *declared_dispatch
            .entry(case.file.clone())
            .or_insert_with(|| package::load_check_config(Some(&case.file)).trusted_host_dispatch);
        case.trusted_host_dispatch = options.trusted_host_dispatch || declared;
    }
    if let Some(shard) = options.shard {
        discovery.cases = select_shard_cases(discovery.cases, &timings, shard);
        if shard.index() > 1 {
            discovery.discovery_errors.clear();
        }
    }
    let skill_contexts = PreparedSkillContexts::prepare(&discovery.cases, &options.cli_skill_dirs);
    let collection_ms = collection_start.elapsed().as_millis() as u64;
    let selected_files_with_tests = if options.shard.is_some() {
        count_files_with_cases(&discovery.cases)
    } else {
        discovery.files_with_tests
    };

    emit_progress(
        &options.progress,
        TestRunEvent::SuiteDiscovered {
            total_tests: discovery.cases.len(),
            total_files: selected_files_with_tests,
            parallel: options.parallel,
            workers,
        },
    );
    if workers == 1
        && should_warn_large_sequential_suite(discovery.cases.len(), selected_files_with_tests)
    {
        emit_progress(
            &options.progress,
            TestRunEvent::LargeSequentialSuite {
                total_tests: discovery.cases.len(),
                total_files: selected_files_with_tests,
            },
        );
    }

    let mut cases = discovery.cases;
    sort_cases_longest_first(&mut cases, &timings);
    let module_preparation = session.prepare_import_graphs(
        cases
            .iter()
            .map(|case| (case.file.clone(), case.trusted_host_dispatch)),
    );

    let mut all_results = discovery.discovery_errors;
    let total_tests = cases.len();
    let callable_preparation = if !options.fail_fast || all_results.is_empty() {
        let prepared = prepare_callable_entries(cases, session);
        cases = prepared.cases;
        all_results.extend(prepared.failures);
        prepared.timing
    } else {
        cases.clear();
        SuiteCallablePreparation::default()
    };
    if !options.fail_fast || all_results.is_empty() {
        let prepared = prepare_file_fixtures(
            cases,
            options,
            session,
            &skill_contexts,
            operator_approval_grant,
        )
        .await;
        cases = prepared.cases;
        all_results.extend(prepared.failures);
    } else {
        cases.clear();
    }
    let execution = if !options.fail_fast || all_results.is_empty() {
        execute_cases(
            cases,
            workers,
            options,
            total_tests,
            session,
            skill_contexts,
            operator_approval_grant,
        )
        .await
    } else {
        CaseExecutionResults::default()
    };

    let timing = DurationSummary::from_samples(
        &execution
            .cases
            .iter()
            .map(|result| result.duration_ms)
            .collect::<Vec<_>>(),
    );
    if let Some(path) = timings_path.as_deref() {
        update_timings_cache(path, timings, &execution.cases);
    }
    all_results.extend(execution.cases);
    all_results.extend(execution.infrastructure_errors);
    let total = all_results.len();
    let passed = all_results.iter().filter(|result| result.passed).count();
    let failed = total - passed;
    let aggregate = AggregateTimings::from_results(
        collection_ms,
        module_preparation,
        callable_preparation,
        &all_results,
    );

    TestSummary {
        results: all_results,
        passed,
        failed,
        total,
        duration_ms: start.elapsed().as_millis() as u64,
        timing,
        aggregate,
    }
}

/// Backwards-compatible single-file API used by `harn dev`.
///
/// Runs every test in one file on the current thread. The new scheduler
/// uses per-test worker threads, but `harn dev` re-runs a single module
/// in the foreground after each rebuild — the queueing machinery would
/// add latency without parallelism to gain back, so we keep this path
/// minimal.
pub async fn run_test_file(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    execution_cwd: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
) -> Result<Vec<TestResult>, String> {
    run_test_file_with_session(
        path,
        filter,
        timeout_ms,
        execution_cwd,
        cli_skill_dirs,
        &TestRunSession::default(),
    )
    .await
}

/// Single-file test API that retains prepared artifacts across invocations.
pub fn run_test_file_with_session<'a>(
    path: &'a Path,
    filter: Option<&'a str>,
    timeout_ms: u64,
    execution_cwd: Option<&'a Path>,
    cli_skill_dirs: &'a [PathBuf],
    session: &'a TestRunSession,
) -> Pin<Box<dyn Future<Output = Result<Vec<TestResult>, String>> + 'a>> {
    Box::pin(run_test_file_with_session_impl(
        path,
        filter,
        timeout_ms,
        execution_cwd,
        cli_skill_dirs,
        session,
    ))
}

async fn run_test_file_with_session_impl(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    execution_cwd: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
    session: &TestRunSession,
) -> Result<Vec<TestResult>, String> {
    let source =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let program = parse_program(&source)?;
    let source = Arc::new(source);
    let program = Arc::new(program);

    let mut cases = extract_cases_from_program(path, &source, &program, filter, usize::MAX)?;
    seed_imported_enum_candidates(path, &source, &mut cases);
    let trusted_host_dispatch = package::load_check_config(Some(path)).trusted_host_dispatch;
    for case in &mut cases {
        case.trusted_host_dispatch = trusted_host_dispatch;
    }
    let skill_contexts = PreparedSkillContexts::prepare(&cases, cli_skill_dirs);
    let _module_preparation = session.prepare_import_graphs(
        cases
            .iter()
            .map(|case| (case.file.clone(), case.trusted_host_dispatch)),
    );

    let mut results = Vec::with_capacity(cases.len());
    let callable_preparation = prepare_callable_entries(cases, session);
    results.extend(callable_preparation.failures);
    let cases = callable_preparation.cases;
    let execution_cwd = execution_cwd
        .map(Path::to_path_buf)
        .unwrap_or_else(test_execution_cwd);
    let prepared_module_cache = session.prepared_module_cache(0);
    let fixture_options = RunOptions {
        timeout_ms,
        ..RunOptions::default()
    };
    let prepared =
        prepare_file_fixtures(cases, &fixture_options, session, &skill_contexts, None).await;
    results.extend(prepared.failures);
    for case in prepared.cases {
        let loaded_skills = skill_contexts.for_case(&case);
        results.push(
            execute_case(
                &case,
                &execution_cwd,
                timeout_ms,
                loaded_skills,
                &prepared_module_cache,
                session.stdio_available(),
                None,
            )
            .await,
        );
    }
    Ok(results)
}

fn resolve_workers(options: &RunOptions) -> usize {
    if !options.parallel {
        return 1;
    }
    // Per-case timing budgets are measurement assertions, not safety
    // timeouts. Running measured cases beside other VMs makes the verdict a
    // function of scheduler contention and turns an otherwise-correct test
    // red on a busy host. Keep the measurement lane serial even when a caller
    // also supplies --parallel/--jobs; the ordinary per-case timeout remains
    // available for bounded parallel correctness runs.
    if options.max_test_ms.is_some() || options.max_execute_ms.is_some() {
        return 1;
    }
    if let Some(jobs) = options.jobs {
        return jobs.max(1);
    }
    if let Ok(raw) = std::env::var(HARN_TEST_JOBS_ENV) {
        if let Ok(parsed) = raw.trim().parse::<usize>() {
            if parsed >= 1 {
                return parsed;
            }
        }
    }
    let detected = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let core_cap = detected.clamp(1, DEFAULT_PARALLEL_JOBS_CAP);
    apply_memory_cap(core_cap)
}

pub(crate) fn resolve_parallel_workers(jobs: Option<usize>) -> usize {
    resolve_workers(&RunOptions {
        parallel: true,
        jobs,
        ..RunOptions::default()
    })
}

/// Lower `core_cap` to what currently-available system memory can hold, so an
/// auto-sized parallel suite backs off on a loaded or small host instead of
/// overcommitting RAM. Returns `core_cap` unchanged when memory is plentiful
/// or cannot be measured. Emits a one-line notice when the cap bites so CI
/// logs explain the reduced parallelism.
fn apply_memory_cap(core_cap: usize) -> usize {
    let Some(available_mb) = available_memory_mb() else {
        return core_cap;
    };
    let budget = per_worker_memory_mb();
    let mem_cap = memory_worker_cap(available_mb, budget, RESERVED_SYSTEM_MEMORY_MB);
    if mem_cap < core_cap {
        eprintln!(
            "harn test: capping workers {core_cap} -> {mem_cap} \
             (~{available_mb} MiB available, {budget} MiB/worker; \
             override with --jobs / HARN_TEST_JOBS)"
        );
        return mem_cap;
    }
    core_cap
}

/// Pure worker-count-from-memory math, factored out so it is unit-testable
/// without touching the host. Always yields at least one worker.
fn memory_worker_cap(available_mb: u64, per_worker_mb: u64, reserved_mb: u64) -> usize {
    let usable = available_mb.saturating_sub(reserved_mb);
    let per_worker = per_worker_mb.max(1);
    ((usable / per_worker).max(1)) as usize
}

/// Per-worker memory budget, honoring the `HARN_TEST_WORKER_MEMORY_MB`
/// override (values `>= 1`), else [`DEFAULT_WORKER_MEMORY_MB`].
fn per_worker_memory_mb() -> u64 {
    std::env::var(HARN_TEST_WORKER_MEMORY_MB_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_WORKER_MEMORY_MB)
}

/// Best-effort "memory available for new work" in MiB: the lesser of the
/// host's available memory and (on Linux) this process's cgroup-v2 headroom.
///
/// Host memory comes from `sysinfo`, so it is correct on Linux, macOS, and
/// Windows. The cgroup min means a container or a systemd-sliced CI runner
/// sizes to its *slice* rather than the whole host — the key to stopping two
/// runner agents on one box from each sizing to ~100% and collectively
/// overcommitting RAM (the "thundering herd" behind the self-hosted
/// runner-loss cancellations). Returns `None` when nothing can be measured,
/// leaving the core-based cap in force.
fn available_memory_mb() -> Option<u64> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let host_mb = match sys.available_memory() {
        0 => None, // unsupported / detection failed — don't over-throttle
        bytes => Some(bytes / (1024 * 1024)),
    };
    match (host_mb, cgroup_v2_headroom_mb()) {
        (Some(h), Some(c)) => Some(h.min(c)),
        (Some(h), None) => Some(h),
        (None, c) => c,
    }
}

/// cgroup-v2 memory headroom (MiB) for this process's own cgroup, or `None`
/// when not on cgroup v2, no limit is set, or the files cannot be read.
#[cfg(target_os = "linux")]
fn cgroup_v2_headroom_mb() -> Option<u64> {
    let dir = own_cgroup_v2_dir()?;
    let max_raw = fs::read_to_string(dir.join("memory.max")).ok()?;
    let current_raw = fs::read_to_string(dir.join("memory.current")).ok()?;
    cgroup_headroom_mb(&max_raw, &current_raw)
}

#[cfg(not(target_os = "linux"))]
fn cgroup_v2_headroom_mb() -> Option<u64> {
    None
}

/// Resolve this process's own cgroup-v2 directory under `/sys/fs/cgroup` from
/// the unified-hierarchy line (`0::<path>`) in `/proc/self/cgroup`. A limit
/// set directly on a systemd service slice or on a container's namespaced
/// root lives here; ancestor-only limits are not chased (the host min still
/// backstops those). `None` on cgroup v1 / hybrid (no `0::` line).
#[cfg(target_os = "linux")]
fn own_cgroup_v2_dir() -> Option<PathBuf> {
    let content = fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = content
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim();
    let rel = rel.strip_prefix('/').unwrap_or(rel);
    Some(Path::new("/sys/fs/cgroup").join(rel))
}

/// Pure headroom math from raw `memory.max` / `memory.current` file contents
/// (both bytes; `memory.max` may be the literal `"max"` sentinel = unlimited).
/// `memory.current` counts reclaimable page cache, so the result is a
/// conservative (under-)estimate of true headroom — the safe direction for
/// OOM avoidance.
#[cfg(any(target_os = "linux", test))]
fn cgroup_headroom_mb(memory_max: &str, memory_current: &str) -> Option<u64> {
    let max = memory_max.trim();
    if max == "max" {
        return None;
    }
    let max: u64 = max.parse().ok()?;
    let current: u64 = memory_current.trim().parse().ok()?;
    Some(max.saturating_sub(current) / (1024 * 1024))
}

struct Discovery {
    cases: Vec<TestCase>,
    files_with_tests: usize,
    discovery_errors: Vec<TestResult>,
}

fn discover_test_cases(files: &[PathBuf], filter: Option<&str>, workers: usize) -> Discovery {
    let mut cases = Vec::new();
    let mut files_with_tests = 0usize;
    let mut discovery_errors = Vec::new();

    for file in files {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                discovery_errors.push(TestResult {
                    name: "<file error>".to_string(),
                    file: file.display().to_string(),
                    passed: false,
                    error: Some(format!("Failed to read {}: {e}", file.display())),
                    captured_output: None,
                    timeout: None,
                    duration_ms: 0,
                    phases: None,
                });
                continue;
            }
        };

        let program = match parse_program(&source) {
            Ok(p) => p,
            Err(e) => {
                discovery_errors.push(TestResult {
                    name: "<file error>".to_string(),
                    file: file.display().to_string(),
                    passed: false,
                    error: Some(e),
                    captured_output: None,
                    timeout: None,
                    duration_ms: 0,
                    phases: None,
                });
                continue;
            }
        };

        let source = Arc::new(source);
        let program = Arc::new(program);
        match extract_cases_from_program(file, &source, &program, filter, workers) {
            Ok(mut file_cases) => {
                if !file_cases.is_empty() {
                    seed_imported_enum_candidates(file, &source, &mut file_cases);
                    files_with_tests += 1;
                    cases.extend(file_cases);
                }
            }
            Err(error) => discovery_errors.push(TestResult {
                name: "<file error>".to_string(),
                file: file.display().to_string(),
                passed: false,
                error: Some(error),
                captured_output: None,
                timeout: None,
                duration_ms: 0,
                phases: None,
            }),
        }
    }

    Discovery {
        cases,
        files_with_tests,
        discovery_errors,
    }
}

fn sort_cases_longest_first(cases: &mut [TestCase], timings: &BTreeMap<String, u64>) {
    // Sort ascending so the slowest tests sit at the tail and get popped
    // first by workers. New (unmeasured) tests share the bottom of the
    // queue alongside the fastest known ones — they'll appear in stable
    // file/name order, and once they get their first timing they'll
    // float up to where they belong.
    cases.sort_by(|a, b| {
        let key_a = timings_key(&a.file, &a.name);
        let key_b = timings_key(&b.file, &b.name);
        let dur_a = timings.get(&key_a).copied().unwrap_or(0);
        let dur_b = timings.get(&key_b).copied().unwrap_or(0);
        dur_a
            .cmp(&dur_b)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn select_shard_cases(
    cases: Vec<TestCase>,
    timings: &BTreeMap<String, u64>,
    shard: TestShard,
) -> Vec<TestCase> {
    if shard.total() <= 1 {
        return cases;
    }

    let mut ranked = cases.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        estimated_case_cost_ms(b, timings)
            .cmp(&estimated_case_cost_ms(a, timings))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut buckets = (0..shard.total()).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut costs = vec![0u64; shard.total()];
    let mut counts = vec![0usize; shard.total()];

    for case in ranked {
        let bucket_index = (0..shard.total())
            .min_by_key(|&index| (costs[index], counts[index], index))
            .unwrap_or(0);
        costs[bucket_index] =
            costs[bucket_index].saturating_add(estimated_case_cost_ms(&case, timings));
        counts[bucket_index] += 1;
        buckets[bucket_index].push(case);
    }

    buckets.swap_remove(shard.index() - 1)
}

fn estimated_case_cost_ms(case: &TestCase, timings: &BTreeMap<String, u64>) -> u64 {
    timings
        .get(&timings_key(&case.file, &case.name))
        .copied()
        .unwrap_or(case.weight as u64)
        .max(1)
}

fn count_files_with_cases(cases: &[TestCase]) -> usize {
    let mut files = HashSet::new();
    for case in cases {
        files.insert(case.file.as_path());
    }
    files.len()
}

fn timings_key(file: &Path, name: &str) -> String {
    format!("{}::{}", file.display(), name)
}

fn timings_cache_path(target: &Path) -> Option<PathBuf> {
    // Anchor the cache at the project root if discoverable, otherwise at
    // the directory the suite was launched from. The cache is shared
    // across runs in the same workspace, so a per-suite cache would
    // fragment timings whenever a user runs a subset.
    let probe_root = if target.is_dir() {
        target.to_path_buf()
    } else {
        target.parent()?.to_path_buf()
    };
    let root = harn_vm::stdlib::process::find_project_root(&probe_root)
        .unwrap_or_else(|| probe_root.clone());
    Some(root.join(TIMINGS_CACHE_RELATIVE_PATH))
}

fn load_timings_cache(path: &Path) -> BTreeMap<String, u64> {
    let Ok(contents) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, u64>>(&contents).unwrap_or_default()
}

fn update_timings_cache(path: &Path, mut existing: BTreeMap<String, u64>, results: &[TestResult]) {
    for result in results {
        existing.insert(
            timings_key(Path::new(&result.file), &result.name),
            result.duration_ms,
        );
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(serialized) = serde_json::to_string(&existing) {
        let _ = fs::write(path, serialized);
    }
}

#[derive(Default)]
struct CaseExecutionResults {
    cases: Vec<TestResult>,
    infrastructure_errors: Vec<TestResult>,
}

struct PreparedFixtureCases {
    cases: Vec<TestCase>,
    failures: Vec<TestResult>,
}

async fn prepare_file_fixtures(
    cases: Vec<TestCase>,
    options: &RunOptions,
    session: &TestRunSession,
    skill_contexts: &PreparedSkillContexts,
    operator_approval_grant: Option<&harn_vm::orchestration::OperatorApprovalGrant>,
) -> PreparedFixtureCases {
    let mut values: BTreeMap<(PathBuf, String), Result<IsolateValue, TestResult>> = BTreeMap::new();
    let mut prepared = Vec::with_capacity(cases.len());
    let mut failures = Vec::new();
    let prepared_module_cache = session.prepared_module_cache(0);

    for mut case in cases {
        let Some(fixture) = case
            .fixture
            .as_ref()
            .filter(|fixture| fixture.scope == FixtureScope::File)
            .cloned()
        else {
            prepared.push(case);
            continue;
        };
        let key = (case.file.clone(), fixture.name.clone());
        if !values.contains_key(&key) {
            let cwd = case_execution_cwd(&case);
            let value = execute_file_fixture(
                &case,
                &fixture,
                &cwd,
                options.timeout_ms,
                skill_contexts.for_case(&case),
                &prepared_module_cache,
                session.stdio_available(),
                operator_approval_grant,
            )
            .await;
            if let Err(failure) = &value {
                failures.push(failure.clone());
            }
            values.insert(key.clone(), value);
        }
        match values.get(&key).expect("fixture result inserted above") {
            Ok(value) => {
                case.file_fixture_value = Some(value.clone());
                prepared.push(case);
            }
            Err(_) if options.fail_fast => {
                prepared.clear();
                break;
            }
            Err(_) => {}
        }
    }

    PreparedFixtureCases {
        cases: prepared,
        failures,
    }
}

async fn execute_cases(
    cases: Vec<TestCase>,
    workers: usize,
    options: &RunOptions,
    total_tests: usize,
    session: &TestRunSession,
    skill_contexts: PreparedSkillContexts,
    operator_approval_grant: Option<&harn_vm::orchestration::OperatorApprovalGrant>,
) -> CaseExecutionResults {
    if cases.is_empty() {
        return CaseExecutionResults::default();
    }
    let completed = Arc::new(Mutex::new(0usize));
    if workers <= 1 {
        let prepared_module_cache = session.prepared_module_cache(0);
        let mut results = Vec::with_capacity(cases.len());
        for case in cases {
            let loaded_skills = skill_contexts.for_case(&case);
            let cwd = case_execution_cwd(&case);
            let test_index = next_test_index(&completed);
            emit_progress(
                &options.progress,
                TestRunEvent::TestStarted {
                    name: case.name.clone(),
                    file: case.file.display().to_string(),
                    test_index,
                    total_tests,
                },
            );
            let result = execute_case(
                &case,
                &cwd,
                options.timeout_ms,
                loaded_skills,
                &prepared_module_cache,
                session.stdio_available(),
                operator_approval_grant,
            )
            .await;
            let result = enforce_case_budgets(result, options.max_test_ms, options.max_execute_ms);
            if options.diagnose {
                result.emit_diagnose();
            }
            emit_progress(
                &options.progress,
                TestRunEvent::TestFinished(result.clone()),
            );
            results.push(result);
            if options.fail_fast && !results.last().is_some_and(|result| result.passed) {
                break;
            }
        }
        return CaseExecutionResults {
            cases: results,
            infrastructure_errors: Vec::new(),
        };
    }

    let skill_contexts = Arc::new(skill_contexts);
    let prepared_module_caches = (0..workers)
        .map(|worker_idx| session.prepared_module_cache(worker_idx))
        .collect::<Vec<_>>();
    let stdio_available = session.stdio_available();
    let operator_approval_grant = operator_approval_grant.cloned();
    let timeout_ms = options.timeout_ms;
    let max_test_ms = options.max_test_ms;
    let max_execute_ms = options.max_execute_ms;
    let diagnose = options.diagnose;
    let parallel = harn_test_runner::execute_parallel_cases(
        cases,
        harn_test_runner::ParallelRunOptions {
            workers,
            total_tests,
            stack_size: CLI_RUNTIME_STACK_SIZE,
            fail_fast: options.fail_fast,
            progress: options.progress.clone(),
        },
        move |worker_idx| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("failed to start test runtime: {error}"))?;
            Ok((runtime, prepared_module_caches[worker_idx].clone()))
        },
        move |worker, case| {
            let cwd = case_execution_cwd(case);
            let loaded_skills = skill_contexts.for_case(case);
            let result = worker.0.block_on(execute_case(
                case,
                &cwd,
                timeout_ms,
                loaded_skills,
                &worker.1,
                stdio_available,
                operator_approval_grant.as_ref(),
            ));
            let result = enforce_case_budgets(result, max_test_ms, max_execute_ms);
            if diagnose {
                result.emit_diagnose();
            }
            result
        },
    );
    CaseExecutionResults {
        cases: parallel.cases,
        infrastructure_errors: parallel.infrastructure_errors,
    }
}

fn enforce_case_budgets(
    mut result: TestResult,
    max_test_ms: Option<u64>,
    max_execute_ms: Option<u64>,
) -> TestResult {
    if !result.passed {
        return result;
    }

    let phases = result
        .phases
        .expect("passed test results always carry measured phases");
    let mut violations = Vec::new();
    if let Some(max_ms) = max_test_ms {
        if result.duration_ms > max_ms {
            violations.push(format!(
                "exceeded test wall-clock budget: {}ms > {}ms",
                result.duration_ms, max_ms
            ));
        }
    }
    if let Some(max_ms) = max_execute_ms {
        if phases.execute_ms > max_ms {
            violations.push(format!(
                "exceeded test execute budget: {}ms > {}ms",
                phases.execute_ms, max_ms
            ));
        }
    }

    if violations.is_empty() {
        return result;
    }

    violations.push(format!(
        "phase timings: setup={}ms compile={}ms execute={}ms teardown={}ms total={}ms",
        phases.setup_ms,
        phases.compile_ms,
        phases.execute_ms,
        phases.teardown_ms,
        result.duration_ms
    ));
    result.passed = false;
    result.error = Some(violations.join("\n"));
    result
}

fn next_test_index(counter: &Mutex<usize>) -> usize {
    let mut guard = counter.lock().unwrap();
    *guard += 1;
    *guard
}

fn case_execution_cwd(case: &TestCase) -> PathBuf {
    case.file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(test_execution_cwd)
}

fn discover_test_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(discover_test_files(&path));
            } else if path.extension().is_some_and(|e| e == "harn") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if content.contains("test_") || content.contains("@test") {
                        files.push(canonicalize_existing_path(&path));
                    }
                }
            }
        }
    }
    files.sort();
    files
}
