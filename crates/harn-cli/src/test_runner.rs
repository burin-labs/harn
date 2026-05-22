use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use harn_lexer::Lexer;
use harn_parser::{Attribute, Node, Parser, SNode};

use crate::env_guard::ScopedEnvVar;

#[derive(Clone, Debug)]
pub struct TestResult {
    pub name: String,
    pub file: String,
    pub passed: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct TestSummary {
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub duration_ms: u64,
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

/// Options that shape how a user-test suite is discovered and executed.
///
/// Held separately from the positional path so call sites (one-shot run,
/// `--watch`, persona doctor) can share the same scheduler without
/// keyword-argument explosion at the call sites.
#[derive(Clone, Default)]
pub struct RunOptions {
    pub filter: Option<String>,
    pub timeout_ms: u64,
    /// When false, the scheduler runs with a single worker, preserving the
    /// historical "everything sequential" semantics that `harn test`
    /// defaulted to before `--parallel` was introduced.
    pub parallel: bool,
    /// Explicit worker limit (`-j`/`--jobs`). `None` defaults to the
    /// available parallelism, capped by a small constant when running in
    /// parallel mode. Ignored when `parallel = false`.
    pub jobs: Option<usize>,
    pub cli_skill_dirs: Vec<PathBuf>,
    /// Optional progress callback. When set, the runner emits events as
    /// the suite progresses; consumers (CLI, dev mode) render output.
    pub progress: Option<TestRunProgress>,
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
    source: Arc<String>,
    program: Arc<Vec<SNode>>,
    /// Optional serial group — tests with the same group never run
    /// concurrently with each other, even if workers are idle. Used for
    /// shared fixtures.
    serial_group: Option<String>,
    /// Number of workers this test reserves while running. Capped at the
    /// pool size during discovery so heavy tests still get scheduled.
    weight: usize,
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
        parallel,
        jobs: None,
        cli_skill_dirs: cli_skill_dirs.to_vec(),
        progress: None,
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
        parallel,
        jobs: None,
        cli_skill_dirs: cli_skill_dirs.to_vec(),
        progress,
    };
    run_tests_with_options(path, &options).await
}

/// Run tests with full control over scheduling, worker count, and
/// progress reporting. Workers and scheduling mode are reported via
/// `TestRunEvent::SuiteDiscovered` so consumers can render their own
/// banner instead of the runner printing to stdout directly.
pub async fn run_tests_with_options(path: &Path, options: &RunOptions) -> TestSummary {
    // Default LLM provider to "mock" in test mode unless caller overrides.
    let _default_llm_provider = ScopedEnvVar::set_if_unset("HARN_LLM_PROVIDER", "mock");
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");

    let start = Instant::now();

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

    let discovery = discover_test_cases(&files, options.filter.as_deref(), workers);

    emit_progress(
        &options.progress,
        TestRunEvent::SuiteDiscovered {
            total_tests: discovery.cases.len(),
            total_files: discovery.files_with_tests,
            parallel: options.parallel,
            workers,
        },
    );
    if workers == 1
        && should_warn_large_sequential_suite(discovery.cases.len(), discovery.files_with_tests)
    {
        emit_progress(
            &options.progress,
            TestRunEvent::LargeSequentialSuite {
                total_tests: discovery.cases.len(),
                total_files: discovery.files_with_tests,
            },
        );
    }

    let mut cases = discovery.cases;
    sort_cases_longest_first(&mut cases, &timings);

    let mut all_results = discovery.discovery_errors;
    let total_tests = cases.len();
    all_results.extend(execute_cases(cases, workers, options, total_tests).await);

    let total = all_results.len();
    let passed = all_results.iter().filter(|r| r.passed).count();
    let failed = total - passed;

    if let Some(path) = timings_path.as_deref() {
        update_timings_cache(path, timings, &all_results);
    }

    TestSummary {
        results: all_results,
        passed,
        failed,
        total,
        duration_ms: start.elapsed().as_millis() as u64,
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
    let source =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let program = parse_program(&source)?;
    let source = Arc::new(source);
    let program = Arc::new(program);

    let cases = extract_cases_from_program(path, &source, &program, filter, usize::MAX);

    let mut results = Vec::with_capacity(cases.len());
    let execution_cwd = execution_cwd
        .map(Path::to_path_buf)
        .unwrap_or_else(test_execution_cwd);
    for case in cases {
        results.push(execute_case(&case, &execution_cwd, timeout_ms, cli_skill_dirs).await);
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
    detected.clamp(1, DEFAULT_PARALLEL_JOBS_CAP)
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
                    duration_ms: 0,
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
                    duration_ms: 0,
                });
                continue;
            }
        };

        let source = Arc::new(source);
        let program = Arc::new(program);
        let file_cases = extract_cases_from_program(file, &source, &program, filter, workers);
        if !file_cases.is_empty() {
            files_with_tests += 1;
            cases.extend(file_cases);
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
) -> Vec<TestCase> {
    let mut cases = Vec::new();
    for snode in program.iter() {
        let Some(meta) = inspect_test_pipeline(snode) else {
            continue;
        };
        if let Some(pattern) = filter {
            if !meta.name.contains(pattern) {
                continue;
            }
        }
        // Cap heavy weight so a single annotated test never deadlocks
        // when the pool is smaller than the requested concurrency.
        let weight = meta.weight.min(workers).max(1);
        cases.push(TestCase {
            file: file.to_path_buf(),
            name: meta.name,
            source: Arc::clone(source),
            program: Arc::clone(program),
            serial_group: meta.serial_group,
            weight,
        });
    }
    cases
}

struct PipelineMeta {
    name: String,
    serial_group: Option<String>,
    weight: usize,
}

fn inspect_test_pipeline(snode: &SNode) -> Option<PipelineMeta> {
    // Pipelines marked `@test`, or named `test_*`, are user tests. The
    // companion attributes `@serial` and `@heavy` only tune the scheduler
    // and never make a non-test pipeline discoverable on their own.
    let (attributes, inner) = match &snode.node {
        Node::AttributedDecl { attributes, inner } => (attributes.as_slice(), inner.as_ref()),
        _ => (&[][..], snode),
    };
    let name = match &inner.node {
        Node::Pipeline { name, .. } => name.clone(),
        _ => return None,
    };
    let has_test_attr = attributes.iter().any(|a| a.name == "test");
    if !has_test_attr && !name.starts_with("test_") {
        return None;
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
    Some(PipelineMeta {
        name,
        serial_group,
        weight,
    })
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
        if result.name == "<file error>" || result.name == "<join error>" {
            continue;
        }
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

async fn execute_cases(
    cases: Vec<TestCase>,
    workers: usize,
    options: &RunOptions,
    total_tests: usize,
) -> Vec<TestResult> {
    if cases.is_empty() {
        return Vec::new();
    }
    let completed = Arc::new(Mutex::new(0usize));
    if workers <= 1 {
        let mut results = Vec::with_capacity(cases.len());
        for case in cases {
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
            let result =
                execute_case(&case, &cwd, options.timeout_ms, &options.cli_skill_dirs).await;
            emit_progress(
                &options.progress,
                TestRunEvent::TestFinished(result.clone()),
            );
            results.push(result);
        }
        return results;
    }

    let queue = Arc::new(Mutex::new(cases));
    let gate = Arc::new(ResourceGate::new(workers));
    let results: Arc<Mutex<Vec<TestResult>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::with_capacity(workers);
    for worker_idx in 0..workers {
        let queue = Arc::clone(&queue);
        let gate = Arc::clone(&gate);
        let results = Arc::clone(&results);
        let completed = Arc::clone(&completed);
        let timeout_ms = options.timeout_ms;
        let cli_skill_dirs = options.cli_skill_dirs.clone();
        let progress = options.progress.clone();
        let handle = thread::Builder::new()
            .name(format!("harn-test-worker-{worker_idx}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(error) => {
                        results.lock().unwrap().push(TestResult {
                            name: "<worker error>".to_string(),
                            file: String::new(),
                            passed: false,
                            error: Some(format!("failed to start test runtime: {error}")),
                            duration_ms: 0,
                        });
                        return;
                    }
                };
                // Cases are sorted ascending by historical duration; popping
                // from the tail gives this worker the slowest unclaimed
                // test, which front-loads long poles and prevents workers
                // from stranding on quick tests at the end of the run.
                while let Some(case) = queue.lock().unwrap().pop() {
                    let _guard = gate.acquire(case.weight, case.serial_group.as_deref());
                    let cwd = case_execution_cwd(&case);
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
                    let result =
                        runtime.block_on(execute_case(&case, &cwd, timeout_ms, &cli_skill_dirs));
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
    Arc::try_unwrap(results)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone())
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
            let group_free = group.is_none_or(|g| !state.busy_groups.contains(g));
            if state.available >= weight && group_free {
                state.available -= weight;
                if let Some(g) = group {
                    state.busy_groups.insert(g.to_string());
                }
                return GateGuard {
                    gate: self,
                    weight,
                    group: group.map(str::to_owned),
                };
            }
            state = self.cond.wait(state).unwrap();
        }
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

async fn execute_case(
    case: &TestCase,
    execution_cwd: &Path,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) -> TestResult {
    harn_vm::reset_thread_local_state();

    let start = Instant::now();

    let chunk = match harn_vm::Compiler::new().compile_named(&case.program, &case.name) {
        Ok(c) => c,
        Err(e) => {
            return TestResult {
                name: case.name.clone(),
                file: case.file.display().to_string(),
                passed: false,
                error: Some(format!("Compile error: {e}")),
                duration_ms: 0,
            };
        }
    };

    let local = tokio::task::LocalSet::new();
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let file_display = case.file.display().to_string();
    let result = tokio::time::timeout(
        timeout,
        local.run_until(async {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            crate::install_default_hostlib(&mut vm);
            let source_parent = case.file.parent().unwrap_or(Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            let store_base = project_root.as_deref().unwrap_or(source_parent);
            let source_dir = source_parent.to_string_lossy().into_owned();
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            let pipeline_name = case
                .file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
            vm.set_source_info(&file_display, &case.source);
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(execution_cwd.to_string_lossy().into_owned()),
                    source_dir: Some(source_dir),
                    env: BTreeMap::new(),
                    adapter: None,
                    repo_path: None,
                    worktree_path: None,
                    branch: None,
                    base_ref: None,
                    cleanup: None,
                },
            ));
            if let Some(ref root) = project_root {
                vm.set_project_root(root);
            }
            if let Some(parent) = case.file.parent() {
                if !parent.as_os_str().is_empty() {
                    vm.set_source_dir(parent);
                }
            }
            let loaded =
                crate::skill_loader::load_skills(&crate::skill_loader::SkillLoaderInputs {
                    cli_dirs: cli_skill_dirs.to_vec(),
                    source_path: Some(case.file.clone()),
                });
            crate::skill_loader::emit_loader_warnings(&loaded.loader_warnings);
            crate::skill_loader::install_skills_global(&mut vm, &loaded);
            let extensions = crate::package::load_runtime_extensions(&case.file);
            crate::package::install_runtime_extensions(&extensions);
            crate::package::install_manifest_triggers(&mut vm, &extensions)
                .await
                .map_err(|error| format!("failed to install manifest triggers: {error}"))?;
            crate::package::install_manifest_hooks(&mut vm, &extensions)
                .await
                .map_err(|error| format!("failed to install manifest hooks: {error}"))?;
            vm.set_harness(harn_vm::Harness::real());
            let result = match vm.execute(&chunk).await {
                Ok(val) => Ok(val),
                Err(e) => Err(vm.format_runtime_error(&e)),
            };
            harn_vm::egress::reset_egress_policy_for_host();
            result
        }),
    )
    .await;

    let duration = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(_)) => TestResult {
            name: case.name.clone(),
            file: file_display,
            passed: true,
            error: None,
            duration_ms: duration,
        },
        Ok(Err(e)) => TestResult {
            name: case.name.clone(),
            file: file_display,
            passed: false,
            error: Some(e),
            duration_ms: duration,
        },
        Err(_) => TestResult {
            name: case.name.clone(),
            file: file_display,
            passed: false,
            error: Some(format!("timed out after {timeout_ms}ms")),
            duration_ms: timeout_ms,
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

    struct TempTestDir {
        inner: tempfile::TempDir,
    }

    impl TempTestDir {
        fn new() -> Self {
            Self {
                inner: tempfile::tempdir().unwrap(),
            }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, contents).unwrap();
        }

        fn path(&self) -> &Path {
            self.inner.path()
        }
    }

    #[test]
    fn discover_test_files_returns_canonical_absolute_paths() {
        let temp = TempTestDir::new();
        temp.write("suite/test_alpha.harn", "pipeline test_alpha(task) {}");
        temp.write("suite/nested/test_beta.harn", "pipeline test_beta(task) {}");
        temp.write("suite/annotated.harn", "@test\npipeline annotated(task) {}");
        temp.write("suite/ignore.harn", "pipeline build(task) {}");

        let files = discover_test_files(&temp.path().join("suite"));

        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|path| path.is_absolute()));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/test_alpha.harn")));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/nested/test_beta.harn")));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/annotated.harn")));
    }

    #[tokio::test]
    async fn run_tests_uses_file_parent_as_execution_cwd_and_restores_shell_cwd() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_cwd.harn",
            r#"
pipeline test_current_dir(task) {
  assert_eq(cwd(), source_dir())
}
"#,
        );

        let original_cwd = std::env::current_dir().unwrap();
        let summary = run_tests(&temp.path().join("suite"), None, 1_000, false, &[]).await;
        let restored_cwd = std::env::current_dir().unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.passed, 1);
        assert_eq!(
            fs::canonicalize(restored_cwd).unwrap(),
            fs::canonicalize(original_cwd).unwrap()
        );
    }

    #[tokio::test]
    async fn parallel_run_tests_uses_each_file_parent_as_execution_cwd() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/a/test_one.harn",
            r#"
pipeline test_one(task) {
  assert_eq(cwd(), source_dir())
}
"#,
        );
        temp.write(
            "suite/b/test_two.harn",
            r#"
pipeline test_two(task) {
  assert_eq(cwd(), source_dir())
}
"#,
        );

        let summary = run_tests(&temp.path().join("suite"), None, 1_000, true, &[]).await;
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.passed, 2);
    }

    #[tokio::test]
    async fn run_tests_loads_cli_skill_dirs() {
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "skills/review/SKILL.md",
            r#"---
name: review
short: Review PRs
description: Review pull requests
---

Review instructions.
"#,
        );
        temp.write(
            "suite/test_skills.harn",
            r#"
pipeline test_cli_skills(task) {
  assert_eq(skill_count(skills), 1)
  let found = skill_find(skills, "review")
  assert_eq(found.name, "review")
}
"#,
        );

        let summary = run_tests(
            &temp.path().join("suite"),
            None,
            1_000,
            false,
            &[temp.path().join("skills")],
        )
        .await;

        assert_eq!(summary.failed, 0, "{:?}", summary.results[0].error);
        assert_eq!(summary.passed, 1);
    }

    #[test]
    fn resolve_workers_honors_explicit_jobs() {
        let mut opts = RunOptions::new(1_000);
        opts.parallel = true;
        opts.jobs = Some(3);
        assert_eq!(resolve_workers(&opts), 3);
    }

    #[test]
    fn resolve_workers_returns_one_when_not_parallel() {
        let mut opts = RunOptions::new(1_000);
        opts.parallel = false;
        opts.jobs = Some(8);
        assert_eq!(resolve_workers(&opts), 1);
    }

    #[test]
    fn sort_cases_longest_first_uses_historical_durations() {
        let source = Arc::new(String::new());
        let program = Arc::new(Vec::new());
        let mk = |name: &str| TestCase {
            file: PathBuf::from("tests/a.harn"),
            name: name.to_string(),
            source: Arc::clone(&source),
            program: Arc::clone(&program),
            serial_group: None,
            weight: 1,
        };
        let mut cases = vec![mk("test_quick"), mk("test_slow"), mk("test_medium")];
        let mut timings = BTreeMap::new();
        timings.insert("tests/a.harn::test_slow".to_string(), 5_000);
        timings.insert("tests/a.harn::test_medium".to_string(), 1_000);

        sort_cases_longest_first(&mut cases, &timings);

        // Slowest tests live at the tail so workers pop them first.
        let order: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, vec!["test_quick", "test_medium", "test_slow"]);
    }

    #[test]
    fn resource_gate_serializes_same_group() {
        let gate = Arc::new(ResourceGate::new(4));
        let trace = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let g_a = {
            let trace = Arc::clone(&trace);
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                let _guard = gate.acquire(1, Some("login"));
                trace.lock().unwrap().push("a-start");
                thread::sleep(Duration::from_millis(50));
                trace.lock().unwrap().push("a-end");
            })
        };
        // Give thread A time to grab the lock first.
        thread::sleep(Duration::from_millis(10));
        let g_b = {
            let trace = Arc::clone(&trace);
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                let _guard = gate.acquire(1, Some("login"));
                trace.lock().unwrap().push("b-start");
                trace.lock().unwrap().push("b-end");
            })
        };
        g_a.join().unwrap();
        g_b.join().unwrap();

        let trace = trace.lock().unwrap();
        // B must not start until A ends.
        let a_end = trace.iter().position(|t| *t == "a-end").unwrap();
        let b_start = trace.iter().position(|t| *t == "b-start").unwrap();
        assert!(a_end < b_start, "B started before A finished: {trace:?}");
    }

    #[test]
    fn resource_gate_allows_independent_groups_in_parallel() {
        let gate = Arc::new(ResourceGate::new(4));
        let _guard_a = gate.acquire(1, Some("alpha"));
        // Acquiring beta should not block — the other group is unrelated.
        let acquired = std::sync::Mutex::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                let _ = gate.acquire(1, Some("beta"));
                *acquired.lock().unwrap() = true;
            });
        });
        assert!(*acquired.lock().unwrap());
    }

    #[test]
    fn resource_gate_caps_heavy_weight_at_capacity() {
        // A test that asks for more than the pool size must still be
        // schedulable rather than deadlocking.
        let gate = Arc::new(ResourceGate::new(2));
        let _g = gate.acquire(99, None);
        // Available is now 0; another single-weight task must still wait.
        let started = Arc::new(Mutex::new(false));
        let inner = Arc::clone(&started);
        let gate2 = Arc::clone(&gate);
        let handle = thread::spawn(move || {
            let _guard = gate2.acquire(1, None);
            *inner.lock().unwrap() = true;
        });
        thread::sleep(Duration::from_millis(20));
        assert!(!*started.lock().unwrap(), "should still be waiting");
        drop(_g);
        handle.join().unwrap();
        assert!(*started.lock().unwrap());
    }

    #[tokio::test]
    async fn parallel_scheduler_runs_heavy_tests_without_oversubscribing() {
        // Heavy(2) should never run concurrently with another test when
        // the pool only has two workers — there are no spare permits.
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_heavy.harn",
            r#"
@test
@heavy(threads: 2)
pipeline test_heavy_one(task) {}

@test
pipeline test_light(task) {}
"#,
        );

        let opts = RunOptions {
            filter: None,
            timeout_ms: 5_000,
            parallel: true,
            jobs: Some(2),
            cli_skill_dirs: vec![],
            progress: None,
        };
        let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
        assert_eq!(summary.failed, 0, "{:?}", summary.results);
        assert_eq!(summary.total, 2);
    }

    #[tokio::test]
    async fn parallel_scheduler_handles_serial_group_annotation() {
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_serial.harn",
            r#"
@test
@serial(group: "fixture")
pipeline test_serial_one(task) {}

@test
@serial(group: "fixture")
pipeline test_serial_two(task) {}
"#,
        );

        let opts = RunOptions {
            filter: None,
            timeout_ms: 5_000,
            parallel: true,
            jobs: Some(4),
            cli_skill_dirs: vec![],
            progress: None,
        };
        let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
        assert_eq!(summary.failed, 0, "{:?}", summary.results);
        assert_eq!(summary.passed, 2);
    }

    #[tokio::test]
    async fn parallel_scheduler_persists_timings_cache() {
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_timed.harn",
            r#"
@test
pipeline test_first(task) {}

@test
pipeline test_second(task) {}
"#,
        );

        let opts = RunOptions {
            filter: None,
            timeout_ms: 5_000,
            parallel: true,
            jobs: Some(2),
            cli_skill_dirs: vec![],
            progress: None,
        };
        let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
        assert_eq!(summary.passed, 2);
        let cache = temp.path().join("suite/.harn/test-timings.json");
        assert!(cache.exists(), "expected timings cache at {cache:?}");
        let stored: BTreeMap<String, u64> =
            serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        assert!(
            stored.keys().any(|key| key.contains("test_first")),
            "expected timings for test_first in {stored:?}"
        );
        assert!(
            stored.keys().any(|key| key.contains("test_second")),
            "expected timings for test_second in {stored:?}"
        );
    }

    #[tokio::test]
    async fn parallel_scheduler_emits_progress_events() {
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_events.harn",
            r#"
@test
pipeline test_a(task) {}

@test
pipeline test_b(task) {}
"#,
        );

        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let events_for_progress = Arc::clone(&events);
        let progress: TestRunProgress = Arc::new(move |event| {
            events_for_progress.lock().unwrap().push(match event {
                TestRunEvent::SuiteDiscovered { .. } => "suite",
                TestRunEvent::LargeSequentialSuite { .. } => "large-seq",
                TestRunEvent::TestStarted { .. } => "started",
                TestRunEvent::TestFinished(_) => "finished",
            });
        });
        let opts = RunOptions {
            filter: None,
            timeout_ms: 5_000,
            parallel: true,
            jobs: Some(2),
            cli_skill_dirs: vec![],
            progress: Some(progress),
        };
        let _ = run_tests_with_options(&temp.path().join("suite"), &opts).await;
        let events = events.lock().unwrap();
        assert_eq!(events.first().copied(), Some("suite"));
        assert_eq!(events.iter().filter(|e| **e == "started").count(), 2);
        assert_eq!(events.iter().filter(|e| **e == "finished").count(), 2);
    }
}
