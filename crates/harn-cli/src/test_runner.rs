use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use harn_lexer::Lexer;
use harn_parser::const_eval::{const_eval, ConstEnv, ConstValue};
use harn_parser::{Attribute, Node, Parser, SNode};
use harn_vm::VmValue;
use serde::Serialize;

use crate::env_guard::ScopedEnvVar;
use crate::test_timing::DurationSummary;
use crate::CLI_RUNTIME_STACK_SIZE;

mod execution;
mod reporting;
mod session;
mod skill_context;
#[cfg(test)]
mod tests;

use execution::execute_case;
pub use session::{TestRunSession, TestRunSessionStats};
use skill_context::PreparedSkillContexts;

#[derive(Clone, Debug, Serialize)]
pub struct TestResult {
    pub name: String,
    pub file: String,
    pub passed: bool,
    pub error: Option<String>,
    /// Everything the case wrote via `log`/`print`/`println`/etc, in
    /// execution order. `None` when nothing was written — keeps quiet,
    /// passing cases from padding reports. Discovery and worker-start
    /// errors never reach a VM and always leave this absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_output: Option<String>,
    /// Typed timeout metadata. Consumers must use this instead of parsing
    /// human-readable error text.
    pub timeout: Option<TestTimeout>,
    pub duration_ms: u64,
    /// Per-phase timings for an executed case. Discovery and worker-start
    /// errors have no execution timeline and leave this absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<PhaseTimings>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TestTimeout {
    pub phase: TestPhase,
    pub limit_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestPhase {
    Execute,
}

#[derive(Clone, Debug, Serialize)]
pub struct TestSummary {
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub duration_ms: u64,
    /// Distribution of per-test wall-clock durations.
    pub timing: DurationSummary,
    /// Aggregated phase costs across the entire run.
    pub aggregate: AggregateTimings,
}

/// Wall-clock cost of each phase of a single test execution.
///
/// Sums to the test's `duration_ms` modulo measurement overhead. Surfaced
/// so consumers can attribute cold-start vs assertion cost without
/// having to instrument the runner externally.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct PhaseTimings {
    /// VM construction + stdlib/hostlib registration + skill install +
    /// runtime extension install + manifest hooks/triggers install.
    pub setup_ms: u64,
    /// `Compiler::compile_named` time for this test's chunk.
    pub compile_ms: u64,
    /// `vm.execute(chunk)` wall time, i.e. the actual user-test body.
    pub execute_ms: u64,
    /// VM/LocalSet task cancellation and `reset_thread_local_state` between tests.
    pub teardown_ms: u64,
    /// Module attribution overlapping setup and execute. These values are
    /// diagnostic subtotals and must not be added to the top-level phases.
    pub modules: harn_vm::ModulePhaseStats,
}

/// Cumulative worker-time across the run. Mirrors [`PhaseTimings`] plus the
/// suite-level collection cost (discover + parse). Parallel case phases overlap,
/// so these totals may exceed suite wall time.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct AggregateTimings {
    pub collection_ms: u64,
    pub setup_ms: u64,
    pub compile_ms: u64,
    pub execute_ms: u64,
    pub teardown_ms: u64,
    /// Sum of per-case module attribution. Overlaps setup and execute.
    pub modules: harn_vm::ModulePhaseStats,
}

impl AggregateTimings {
    fn from_results(collection_ms: u64, results: &[TestResult]) -> Self {
        results.iter().filter_map(|result| result.phases).fold(
            Self {
                collection_ms,
                ..Self::default()
            },
            |acc, p| Self {
                collection_ms: acc.collection_ms,
                setup_ms: acc.setup_ms.saturating_add(p.setup_ms),
                compile_ms: acc.compile_ms.saturating_add(p.compile_ms),
                execute_ms: acc.execute_ms.saturating_add(p.execute_ms),
                teardown_ms: acc.teardown_ms.saturating_add(p.teardown_ms),
                modules: acc.modules.saturating_add(p.modules),
            },
        )
    }
}

#[derive(Clone, Debug)]
pub enum TestRunEvent {
    SuiteDiscovered {
        total_tests: usize,
        total_files: usize,
        parallel: bool,
        workers: usize,
    },
    LargeSequentialSuite {
        total_tests: usize,
        total_files: usize,
    },
    TestStarted {
        name: String,
        file: String,
        test_index: usize,
        total_tests: usize,
    },
    TestFinished(TestResult),
}

pub type TestRunProgress = Arc<dyn Fn(TestRunEvent) + Send + Sync>;

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
    /// Test-only setup delay used to prove timeout phase boundaries without
    /// process-global hooks that could leak across concurrent suites.
    #[cfg(test)]
    pub(crate) setup_delay_ms: u64,
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

/// A single executable test discovered during scan. Workers compile and
/// run each case in isolation; the parsed program is shared by `Arc` so
/// large suites parse exactly once.
#[derive(Clone)]
struct TestCase {
    file: PathBuf,
    name: String,
    pipeline_name: String,
    source: Arc<String>,
    program: Arc<Vec<SNode>>,
    /// Optional serial group — tests with the same group never run
    /// concurrently with each other, even if workers are idle. Used for
    /// shared fixtures.
    serial_group: Option<String>,
    /// Number of workers this test reserves while running. Capped at the
    /// pool size during discovery so heavy tests still get scheduled.
    weight: usize,
    /// Parameter bindings supplied by one `@test(cases: [...])` row.
    bindings: Vec<(String, VmValue)>,
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
        #[cfg(test)]
        setup_delay_ms: 0,
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
        #[cfg(test)]
        setup_delay_ms: 0,
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
    Box::pin(run_tests_with_session_impl(path, options, session))
}

async fn run_tests_with_session_impl(
    path: &Path,
    options: &RunOptions,
    session: &TestRunSession,
) -> TestSummary {
    // Default LLM provider to "mock" in test mode unless caller overrides.
    let _default_llm_provider = ScopedEnvVar::set_if_unset("HARN_LLM_PROVIDER", "mock");
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");

    let start = Instant::now();

    let collection_start = Instant::now();
    let canonical_target = canonicalize_existing_path(path);
    let files = if canonical_target.is_dir() {
        discover_test_files(&canonical_target)
    } else {
        vec![canonical_target.clone()]
    };

    let workers = resolve_workers(options);
    let timings_path = timings_cache_path(&canonical_target);
    let timings = timings_path
        .as_deref()
        .map(load_timings_cache)
        .unwrap_or_default();

    let mut discovery = discover_test_cases(&files, options.filter.as_deref(), workers);
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

    let mut all_results = discovery.discovery_errors;
    let total_tests = cases.len();
    let execution = if !options.fail_fast || all_results.is_empty() {
        execute_cases(
            cases,
            workers,
            options,
            total_tests,
            session,
            skill_contexts,
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
    let aggregate = AggregateTimings::from_results(collection_ms, &all_results);

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

    let cases = extract_cases_from_program(path, &source, &program, filter, usize::MAX)?;
    let skill_contexts = PreparedSkillContexts::prepare(&cases, cli_skill_dirs);

    let mut results = Vec::with_capacity(cases.len());
    let execution_cwd = execution_cwd
        .map(Path::to_path_buf)
        .unwrap_or_else(test_execution_cwd);
    let prepared_module_cache = session.prepared_module_cache(0);
    for case in cases {
        let loaded_skills = skill_contexts.for_case(&case);
        results.push(
            execute_case(
                &case,
                &execution_cwd,
                timeout_ms,
                loaded_skills,
                &prepared_module_cache,
                session.stdio_available(),
                #[cfg(test)]
                0,
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
            Ok(file_cases) => {
                if !file_cases.is_empty() {
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

fn parse_program(source: &str) -> Result<Vec<SNode>, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| format!("{e}"))?;
    let mut parser = Parser::new(tokens);
    parser.parse().map_err(|e| format!("{e}"))
}

fn extract_cases_from_program(
    file: &Path,
    source: &Arc<String>,
    program: &Arc<Vec<SNode>>,
    filter: Option<&str>,
    workers: usize,
) -> Result<Vec<TestCase>, String> {
    let mut cases = Vec::new();
    for snode in program.iter() {
        let Some(meta) = inspect_test_pipeline(snode)? else {
            continue;
        };
        // Cap heavy weight so a single annotated test never deadlocks
        // when the pool is smaller than the requested concurrency.
        let weight = meta.weight.min(workers).max(1);
        if meta.rows.is_empty() {
            if filter.is_some_and(|pattern| !meta.name.contains(pattern)) {
                continue;
            }
            cases.push(TestCase {
                file: file.to_path_buf(),
                name: meta.name.clone(),
                pipeline_name: meta.name,
                source: Arc::clone(source),
                program: Arc::clone(program),
                serial_group: meta.serial_group,
                weight,
                bindings: Vec::new(),
            });
        } else {
            for row in meta.rows {
                let case_name = format!("{}[{}]", meta.name, row.name);
                if filter.is_some_and(|pattern| !case_name.contains(pattern)) {
                    continue;
                }
                cases.push(TestCase {
                    file: file.to_path_buf(),
                    name: case_name,
                    pipeline_name: meta.name.clone(),
                    source: Arc::clone(source),
                    program: Arc::clone(program),
                    serial_group: meta.serial_group.clone(),
                    weight,
                    bindings: meta.params.iter().cloned().zip(row.args).collect(),
                });
            }
        }
    }
    Ok(cases)
}

struct PipelineMeta {
    name: String,
    params: Vec<String>,
    serial_group: Option<String>,
    weight: usize,
    rows: Vec<ParameterizedRow>,
}

struct ParameterizedRow {
    name: String,
    args: Vec<VmValue>,
}

fn inspect_test_pipeline(snode: &SNode) -> Result<Option<PipelineMeta>, String> {
    // Pipelines marked `@test`, or named `test_*`, are user tests. The
    // companion attributes `@serial` and `@heavy` only tune the scheduler
    // and never make a non-test pipeline discoverable on their own.
    let (attributes, inner) = match &snode.node {
        Node::AttributedDecl { attributes, inner } => (attributes.as_slice(), inner.as_ref()),
        _ => (&[][..], snode),
    };
    let (name, params) = match &inner.node {
        Node::Pipeline { name, params, .. } => (name.clone(), params.clone()),
        _ => return Ok(None),
    };
    let has_test_attr = attributes.iter().any(|a| a.name == "test");
    if !has_test_attr && !name.starts_with("test_") {
        return Ok(None);
    }
    let serial_group = attributes
        .iter()
        .find(|a| a.name == "serial")
        .map(serial_group_for);
    let weight = attributes
        .iter()
        .find(|a| a.name == "heavy")
        .and_then(heavy_weight_for)
        .unwrap_or(1);
    let rows = match attributes.iter().find(|a| a.name == "test") {
        Some(attribute) => parameterized_rows(attribute, &name, params.len())?,
        None => Vec::new(),
    };
    Ok(Some(PipelineMeta {
        name,
        params,
        serial_group,
        weight,
        rows,
    }))
}

fn parameterized_rows(
    attribute: &Attribute,
    pipeline_name: &str,
    parameter_count: usize,
) -> Result<Vec<ParameterizedRow>, String> {
    let Some(cases) = attribute.named_arg("cases") else {
        return Ok(Vec::new());
    };
    let Node::ListLiteral(items) = &cases.node else {
        return Err(format!(
            "@test cases for `{pipeline_name}` must be a list of {{name, args}} rows"
        ));
    };
    if items.is_empty() {
        return Err(format!(
            "@test cases for `{pipeline_name}` must not be empty"
        ));
    }

    let mut rows = Vec::with_capacity(items.len());
    let mut names = HashSet::new();
    for item in items {
        let Node::DictLiteral(entries) = &item.node else {
            return Err(format!(
                "@test case in `{pipeline_name}` must be a {{name, args}} dict"
            ));
        };
        let name_node = dict_entry(entries, "name").ok_or_else(|| {
            format!("@test case in `{pipeline_name}` is missing string field `name`")
        })?;
        let name = match &name_node.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => value.trim().to_string(),
            _ => {
                return Err(format!(
                    "@test case name in `{pipeline_name}` must be a string literal"
                ));
            }
        };
        if name.is_empty() || !names.insert(name.clone()) {
            return Err(format!(
                "@test case names in `{pipeline_name}` must be non-empty and unique: `{name}`"
            ));
        }
        let args_node = dict_entry(entries, "args").ok_or_else(|| {
            format!("@test case `{name}` in `{pipeline_name}` is missing list field `args`")
        })?;
        let Node::ListLiteral(args) = &args_node.node else {
            return Err(format!(
                "@test case `{name}` in `{pipeline_name}` must provide `args` as a list"
            ));
        };
        if args.len() != parameter_count {
            return Err(format!(
                "@test case `{name}` in `{pipeline_name}` has {} arguments; expected {parameter_count}",
                args.len()
            ));
        }
        let args = args
            .iter()
            .map(attribute_value)
            .collect::<Result<Vec<_>, _>>()?;
        rows.push(ParameterizedRow { name, args });
    }
    Ok(rows)
}

fn dict_entry<'a>(entries: &'a [harn_parser::DictEntry], key: &str) -> Option<&'a SNode> {
    entries.iter().find_map(|entry| {
        let matches = match &entry.key.node {
            Node::Identifier(value) | Node::StringLiteral(value) => value == key,
            _ => false,
        };
        matches.then_some(&entry.value)
    })
}

fn attribute_value(node: &SNode) -> Result<VmValue, String> {
    let value = const_eval(node, &ConstEnv::new())
        .map_err(|error| format!("@test case arguments must be compile-time values: {error:?}"))?;
    Ok(const_value_to_vm(value))
}

fn const_value_to_vm(value: ConstValue) -> VmValue {
    match value {
        ConstValue::Int(value) => VmValue::Int(value),
        ConstValue::Float(value) => VmValue::Float(value),
        ConstValue::Bool(value) => VmValue::Bool(value),
        ConstValue::String(value) => VmValue::String(value.into()),
        ConstValue::Nil => VmValue::Nil,
        ConstValue::List(items) => {
            VmValue::List(Arc::new(items.into_iter().map(const_value_to_vm).collect()))
        }
        ConstValue::Dict(entries) => VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (key, const_value_to_vm(value)))
                .collect::<Vec<(String, VmValue)>>(),
        ),
    }
}

fn serial_group_for(attr: &Attribute) -> String {
    attr.string_arg("group")
        .unwrap_or_else(|| "__default__".to_string())
}

fn heavy_weight_for(attr: &Attribute) -> Option<usize> {
    attr.args
        .iter()
        .find(|a| a.name.as_deref() == Some("threads"))
        .and_then(|a| match &a.value.node {
            Node::IntLiteral(n) if *n >= 1 => Some(*n as usize),
            _ => None,
        })
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

async fn execute_cases(
    cases: Vec<TestCase>,
    workers: usize,
    options: &RunOptions,
    total_tests: usize,
    session: &TestRunSession,
    skill_contexts: PreparedSkillContexts,
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
                #[cfg(test)]
                options.setup_delay_ms,
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

    let queue = Arc::new(Mutex::new(cases));
    let skill_contexts = Arc::new(skill_contexts);
    let gate = Arc::new(ResourceGate::new(workers));
    let results: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));
    let infrastructure_errors: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::with_capacity(workers);
    for worker_idx in 0..workers {
        let queue = Arc::clone(&queue);
        let skill_contexts = Arc::clone(&skill_contexts);
        let gate = Arc::clone(&gate);
        let results = Arc::clone(&results);
        let infrastructure_errors = Arc::clone(&infrastructure_errors);
        let completed = Arc::clone(&completed);
        let timeout_ms = options.timeout_ms;
        #[cfg(test)]
        let setup_delay_ms = options.setup_delay_ms;
        let max_test_ms = options.max_test_ms;
        let max_execute_ms = options.max_execute_ms;
        let progress = options.progress.clone();
        let diagnose = options.diagnose;
        let fail_fast = options.fail_fast;
        let cancelled = Arc::clone(&cancelled);
        let prepared_module_cache = session.prepared_module_cache(worker_idx);
        let stdio_available = session.stdio_available();
        let handle = thread::Builder::new()
            .name(format!("harn-test-worker-{worker_idx}"))
            .stack_size(CLI_RUNTIME_STACK_SIZE)
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(error) => {
                        infrastructure_errors.lock().unwrap().push(TestResult {
                            name: "<worker error>".to_string(),
                            file: String::new(),
                            passed: false,
                            error: Some(format!("failed to start test runtime: {error}")),
                            captured_output: None,
                            timeout: None,
                            duration_ms: 0,
                            phases: None,
                        });
                        return;
                    }
                };
                // Cases are sorted ascending by historical duration; popping
                // from the tail gives this worker the slowest unclaimed
                // test, which front-loads long poles and prevents workers
                // from stranding on quick tests at the end of the run.
                loop {
                    let case = claim_next_case(&queue, &cancelled, fail_fast);
                    let Some(case) = case else { break };
                    let _guard = gate.acquire(case.weight, case.serial_group.as_deref());
                    let cwd = case_execution_cwd(&case);
                    let loaded_skills = skill_contexts.for_case(&case);
                    let test_index = next_test_index(&completed);
                    emit_progress(
                        &progress,
                        TestRunEvent::TestStarted {
                            name: case.name.clone(),
                            file: case.file.display().to_string(),
                            test_index,
                            total_tests,
                        },
                    );
                    let result = runtime.block_on(execute_case(
                        &case,
                        &cwd,
                        timeout_ms,
                        loaded_skills,
                        &prepared_module_cache,
                        stdio_available,
                        #[cfg(test)]
                        setup_delay_ms,
                    ));
                    let result = enforce_case_budgets(result, max_test_ms, max_execute_ms);
                    if fail_fast && !result.passed {
                        cancelled.store(true, Ordering::Release);
                    }
                    if diagnose {
                        result.emit_diagnose();
                    }
                    emit_progress(&progress, TestRunEvent::TestFinished(result.clone()));
                    results.lock().unwrap().push(result);
                }
            })
            .expect("spawning a harn-test worker thread should succeed");
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.join();
    }

    // All workers have joined, so this Arc holds the only remaining
    // reference. The lock-and-clone fallback survives the unlikely case
    // where a panic-unwind kept an extra reference alive.
    let cases = Arc::try_unwrap(results)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone());
    let infrastructure_errors = Arc::try_unwrap(infrastructure_errors)
        .map(|mutex| mutex.into_inner().unwrap_or_default())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone());
    CaseExecutionResults {
        cases,
        infrastructure_errors,
    }
}

fn claim_next_case(
    queue: &Mutex<Vec<TestCase>>,
    cancelled: &AtomicBool,
    fail_fast: bool,
) -> Option<TestCase> {
    let mut queue = queue.lock().unwrap();
    if fail_fast && cancelled.load(Ordering::Acquire) {
        None
    } else {
        queue.pop()
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

/// Coordinates worker permits and serial-group exclusivity without
/// requiring an async lock — workers are dedicated OS threads, so a
/// classic Mutex+Condvar gate keeps everything off the tokio scheduler.
struct ResourceGate {
    state: Mutex<GateState>,
    cond: Condvar,
    capacity: usize,
}

struct GateState {
    available: usize,
    busy_groups: HashSet<String>,
}

struct GateGuard<'a> {
    gate: &'a ResourceGate,
    weight: usize,
    group: Option<String>,
}

impl ResourceGate {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                available: capacity,
                busy_groups: HashSet::new(),
            }),
            cond: Condvar::new(),
            capacity,
        }
    }

    fn acquire(&self, weight: usize, group: Option<&str>) -> GateGuard<'_> {
        let weight = weight.min(self.capacity).max(1);
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(guard) = self.try_grab_locked(&mut state, weight, group) {
                return guard;
            }
            state = self.cond.wait(state).unwrap();
        }
    }

    /// Grab a permit if one is immediately available, holding the already-locked
    /// state. Returns `None` without blocking when the pool is exhausted or the
    /// group is busy. Shared by `acquire` (which retries) and `try_acquire`.
    fn try_grab_locked<'a>(
        &'a self,
        state: &mut GateState,
        weight: usize,
        group: Option<&str>,
    ) -> Option<GateGuard<'a>> {
        let group_free = group.is_none_or(|g| !state.busy_groups.contains(g));
        if state.available >= weight && group_free {
            state.available -= weight;
            if let Some(g) = group {
                state.busy_groups.insert(g.to_string());
            }
            return Some(GateGuard {
                gate: self,
                weight,
                group: group.map(str::to_owned),
            });
        }
        None
    }

    /// Non-blocking variant of `acquire` used by tests to assert gate state
    /// deterministically (in-process, no threads or wall-clock sleeps).
    #[cfg(test)]
    fn try_acquire(&self, weight: usize, group: Option<&str>) -> Option<GateGuard<'_>> {
        let weight = weight.min(self.capacity).max(1);
        let mut state = self.state.lock().unwrap();
        self.try_grab_locked(&mut state, weight, group)
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.available += self.weight;
        if let Some(group) = self.group.as_deref() {
            state.busy_groups.remove(group);
        }
        self.gate.cond.notify_all();
    }
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
