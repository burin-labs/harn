use super::*;

use std::sync::Arc;

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

fn loaded_skills_for(file: &Path) -> crate::skill_loader::LoadedSkills {
    crate::skill_loader::load_skills(&crate::skill_loader::SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: Some(file.to_path_buf()),
    })
}

#[tokio::test]
async fn execution_budget_starts_after_setup_and_stops_cpu_bound_code() {
    let temp = TempTestDir::new();
    temp.write(
        "test_timeout.harn",
        "pipeline test_timeout(_task) { try { while true {} } catch (_error) { return 1 } }",
    );
    let file = temp.path().join("test_timeout.harn");
    let source = Arc::new(fs::read_to_string(&file).unwrap());
    let case = TestCase {
        name: "test_timeout".to_string(),
        pipeline_name: "test_timeout".to_string(),
        program: Arc::new(parse_program(&source).unwrap()),
        source,
        file: file.clone(),
        imported_enum_candidates: Arc::new(Vec::new()),
        bindings: Vec::new(),
        weight: 1,
        serial_group: None,
    };

    let result = execute_case(
        &case,
        temp.path(),
        0,
        &loaded_skills_for(&file),
        &harn_vm::PreparedModuleCache::default(),
        true,
    )
    .await;

    assert!(!result.passed);
    let timeout = result.timeout.expect("expected typed timeout metadata");
    assert_eq!(timeout.phase, TestPhase::Execute);
    assert_eq!(timeout.limit_ms, 0);
    assert_eq!(
        result.phases.expect("measured phases").modules,
        harn_vm::ModulePhaseStats::default()
    );
    assert_eq!(
        result.error.as_deref(),
        Some("execute phase timed out after 0ms")
    );
}

async fn run_single_case(temp: &TempTestDir, name: &str, source_body: &str) -> TestResult {
    let file_name = format!("{name}.harn");
    temp.write(&file_name, source_body);
    let file = temp.path().join(&file_name);
    let source = Arc::new(fs::read_to_string(&file).unwrap());
    let case = TestCase {
        name: name.to_string(),
        pipeline_name: name.to_string(),
        program: Arc::new(parse_program(&source).unwrap()),
        source,
        file: file.clone(),
        imported_enum_candidates: Arc::new(Vec::new()),
        bindings: Vec::new(),
        weight: 1,
        serial_group: None,
    };
    execute_case(
        &case,
        temp.path(),
        30_000,
        &loaded_skills_for(&file),
        &harn_vm::PreparedModuleCache::default(),
        true,
    )
    .await
}

/// `log`/`print`/`println` write into the VM's per-case output buffer
/// (`Vm::output`). That buffer must survive past `drop(vm)` into the
/// `TestResult` for both outcomes below — a passing case's probes are the
/// entire point of adding them, and a failing case is exactly when an
/// author most needs them.
#[tokio::test]
async fn execute_case_captures_log_output_for_a_passing_case() {
    let temp = TempTestDir::new();
    let result = run_single_case(
        &temp,
        "test_probe",
        "pipeline test_probe(_task) { log(\"HELLO_PROBE\"); return 1 }",
    )
    .await;

    assert!(result.passed, "case should pass: {:?}", result.error);
    let output = result
        .captured_output
        .expect("a case that calls log() must carry captured_output");
    assert!(
        output.contains("HELLO_PROBE"),
        "captured output missing log() text: {output:?}"
    );
}

#[tokio::test]
async fn execute_case_captures_log_output_for_a_failing_case() {
    let temp = TempTestDir::new();
    let result = run_single_case(
        &temp,
        "test_probe_fail",
        "pipeline test_probe_fail(_task) { log(\"HELLO_FAIL_PROBE\"); assert(false, \"boom\") }",
    )
    .await;

    assert!(!result.passed);
    assert!(result.error.unwrap_or_default().contains("boom"));
    let output = result
        .captured_output
        .expect("a failing case that calls log() must still carry captured_output");
    assert!(
        output.contains("HELLO_FAIL_PROBE"),
        "captured output missing log() text: {output:?}"
    );
}

#[tokio::test]
async fn execute_case_leaves_captured_output_absent_when_nothing_was_written() {
    let temp = TempTestDir::new();
    let result = run_single_case(
        &temp,
        "test_silent",
        "pipeline test_silent(_task) { return 1 }",
    )
    .await;

    assert!(result.passed);
    assert!(
        result.captured_output.is_none(),
        "a silent case must not carry a captured_output value: {:?}",
        result.captured_output
    );
}

#[tokio::test]
async fn execution_timeout_captures_lazy_module_load_attribution() {
    let temp = TempTestDir::new();
    temp.write("helper.harn", "pub fn spin() { while true {} }\n");
    temp.write(
        "test_timeout_import.harn",
        "import { spin } from \"./helper\"\npipeline test_timeout_import(_task) { spin() }\n",
    );
    let file = temp.path().join("test_timeout_import.harn");
    let source = Arc::new(fs::read_to_string(&file).unwrap());
    let case = TestCase {
        name: "test_timeout_import".to_string(),
        pipeline_name: "test_timeout_import".to_string(),
        program: Arc::new(parse_program(&source).unwrap()),
        source,
        file,
        imported_enum_candidates: Arc::new(Vec::new()),
        bindings: Vec::new(),
        weight: 1,
        serial_group: None,
    };

    let result = execute_case(
        &case,
        temp.path(),
        100,
        &loaded_skills_for(&temp.path().join("test_timeout_import.harn")),
        &harn_vm::PreparedModuleCache::default(),
        true,
    )
    .await;

    assert_eq!(
        result.timeout.expect("typed timeout").phase,
        TestPhase::Execute
    );
    let phases = result.phases.expect("measured phases");
    assert_eq!(phases.modules.modules_loaded, 1);
}

#[tokio::test]
async fn failures_and_timeout_do_not_leak_module_timing_to_later_runs() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write("suite/helper.harn", "pub fn value() { return 42 }\n");
    temp.write(
        "suite/harn.toml",
        r#"
[package]
name = "setup-failure"

[exports]
handlers = "hooks.harn"

[[hooks]]
event = "PostTurn"
handler = "handlers::missing"
"#,
    );
    temp.write("suite/hooks.harn", "pub fn valid(_event) {}\n");
    temp.write(
        "suite/test_setup_failure.harn",
        "pipeline test_setup_failure(_task) {}\n",
    );
    temp.write(
        "suite/test_runtime_failure.harn",
        "import { value } from \"./helper\"\npipeline test_runtime_failure(_task) { throw \"boom\" }\n",
    );
    temp.write(
        "suite/test_timeout.harn",
        "import { value } from \"./helper\"\npipeline test_timeout(_task) { while true {} }\n",
    );
    temp.write(
        "suite/test_pass.harn",
        "import { value } from \"./helper\"\npipeline test_pass(_task) { assert_eq(value(), 42) }\n",
    );
    let options = RunOptions::new(1_000);
    let timeout_options = RunOptions::new(25);
    let session = TestRunSession::default();

    let setup = run_tests_with_session(
        &temp.path().join("suite/test_setup_failure.harn"),
        &options,
        &session,
    )
    .await;
    fs::remove_file(temp.path().join("suite/harn.toml")).expect("remove failing manifest");
    let runtime = run_tests_with_session(
        &temp.path().join("suite/test_runtime_failure.harn"),
        &options,
        &session,
    )
    .await;
    let timeout = run_tests_with_session(
        &temp.path().join("suite/test_timeout.harn"),
        &timeout_options,
        &session,
    )
    .await;
    let pass = run_tests_with_session(
        &temp.path().join("suite/test_pass.harn"),
        &options,
        &session,
    )
    .await;

    let phases = |summary: &TestSummary| {
        summary.results[0]
            .phases
            .expect("executed case has measured phases")
    };
    assert_eq!(phases(&setup).modules.modules_loaded, 0);
    assert!(setup.results[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("failed to install manifest hooks")));
    assert_eq!(phases(&runtime).modules.modules_loaded, 1);
    assert_eq!(runtime.failed, 1);
    assert!(runtime.results[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("boom")));
    assert_eq!(phases(&timeout).modules.modules_loaded, 1);
    assert_eq!(phases(&timeout).modules.modules_compiled, 0);
    assert_eq!(
        timeout.results[0].timeout.expect("typed timeout").phase,
        TestPhase::Execute
    );
    assert_eq!(phases(&pass).modules.modules_loaded, 1);
    assert_eq!(phases(&pass).modules.modules_compiled, 0);
    assert_eq!(pass.passed, 1, "{:?}", pass.results);
}

#[tokio::test]
async fn reusable_session_hits_prepared_cache_without_sharing_module_state() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/counter.harn",
        r"
let count = 0

pub fn increment() {
  count = count + 1
  return count
}
",
    );
    temp.write(
        "suite/test_counter.harn",
        r#"
import { increment } from "./counter"

pipeline test_counter(_task) {
  assert_eq(increment(), 1)
  assert_eq(increment(), 2)
}
"#,
    );
    let suite = temp.path().join("suite/test_counter.harn");
    let options = RunOptions::new(5_000);
    let session = TestRunSession::default();

    let first = run_tests_with_session(&suite, &options, &session).await;
    let after_first = session.stats();
    let second = run_tests_with_session(&suite, &options, &session).await;
    let after_second = session.stats();

    temp.write(
        "suite/counter.harn",
        r"
let count = 40

pub fn increment() {
  count = count + 1
  return count
}
",
    );
    temp.write(
        "suite/test_counter.harn",
        r#"
import { increment } from "./counter"

pipeline test_counter(_task) {
  assert_eq(increment(), 41)
  assert_eq(increment(), 42)
}
"#,
    );
    let after_edit = run_tests_with_session(&suite, &options, &session).await;
    let after_edit_stats = session.stats();

    assert_eq!(first.passed, 1, "{:?}", first.results);
    assert_eq!(second.passed, 1, "{:?}", second.results);
    assert_eq!(after_edit.passed, 1, "{:?}", after_edit.results);
    assert_eq!(after_first.workers, 1);
    assert!(after_first.insertions >= 1, "{after_first:?}");
    assert!(after_second.hits > after_first.hits, "{after_second:?}");
    assert_eq!(after_second.insertions, after_first.insertions);
    assert!(
        after_edit_stats.insertions > after_second.insertions,
        "{after_edit_stats:?}"
    );
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
        r"
pipeline test_current_dir(task) {
  assert_eq(cwd(), source_dir())
}
",
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
        r"
pipeline test_one(task) {
  assert_eq(cwd(), source_dir())
}
",
    );
    temp.write(
        "suite/b/test_two.harn",
        r"
pipeline test_two(task) {
  assert_eq(cwd(), source_dir())
}
",
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
        r"---
name: review
short: Review PRs
description: Review pull requests
---

Review instructions.
",
    );
    temp.write(
        "suite/test_skills.harn",
        r#"
pipeline test_cli_skills(task) {
  assert_eq(skill_count(skills), 1)
  const found = skill_find(skills, "review")
  assert_eq(found.name, "review")
}

pipeline test_cli_skills_again(task) {
  assert_eq(skill_count(skills), 1)
  const found = skill_find(skills, "review")
  assert_eq(found.name, "review")
}
"#,
    );

    crate::skill_loader::reset_load_skills_calls();
    let summary = run_tests(
        &temp.path().join("suite"),
        None,
        1_000,
        true,
        &[temp.path().join("skills")],
    )
    .await;

    assert_eq!(summary.failed, 0, "{:?}", summary.results[0].error);
    assert_eq!(summary.passed, 2);
    assert_eq!(
        crate::skill_loader::load_skills_calls(),
        1,
        "cases in one source directory must share one discovery result"
    );
}

#[tokio::test]
async fn user_tests_default_to_memory_event_log() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let _backend_guard = ScopedEnvVar::unset(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV);
    let _dir_guard = ScopedEnvVar::unset(harn_vm::event_log::HARN_EVENT_LOG_DIR_ENV);
    let _sqlite_guard = ScopedEnvVar::unset(harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV);
    let _state_guard = ScopedEnvVar::unset(harn_vm::runtime_paths::HARN_STATE_DIR_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_store.harn",
        r#"
pipeline test_store_builtin_uses_runner_event_log(task) {
  store_set("test.key", "value")
  assert_eq(store_get("test.key"), "value")
}
"#,
    );

    let suite = temp.path().join("suite");
    let summary = run_tests(&suite, None, 1_000, false, &[]).await;

    assert_eq!(summary.failed, 0, "{:?}", summary.results[0].error);
    assert_eq!(summary.passed, 1);
    assert!(
        !suite.join(".harn/events.sqlite").exists(),
        "plain user tests should not create the default SQLite event log",
    );
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
fn memory_worker_cap_backs_off_under_pressure() {
    // ~4 GiB available, 1 GiB reserved, 1 GiB/worker -> 3 workers.
    assert_eq!(memory_worker_cap(4096, 1024, 1024), 3);
}

#[test]
fn memory_worker_cap_is_generous_when_memory_is_plentiful() {
    // A roomy box dwarfs the core cap; resolve_workers then min()s this
    // against DEFAULT_PARALLEL_JOBS_CAP, so the core cap stays in force.
    assert!(memory_worker_cap(32_768, 1024, 1024) >= DEFAULT_PARALLEL_JOBS_CAP);
}

#[test]
fn memory_worker_cap_never_starves_to_zero() {
    // Even when reserved >= available, at least one worker must run.
    assert_eq!(memory_worker_cap(512, 1024, 1024), 1);
}

#[test]
fn cgroup_headroom_unlimited_is_none() {
    // The `max` sentinel means no cgroup limit -> defer to host memory.
    assert_eq!(cgroup_headroom_mb("max\n", "1048576\n"), None);
}

#[test]
fn cgroup_headroom_computes_slice_remainder() {
    // 4 GiB limit, 1 GiB in use -> 3 GiB (3072 MiB) headroom.
    let four_gib = (4_u64 * 1024 * 1024 * 1024).to_string();
    let one_gib = (1024_u64 * 1024 * 1024).to_string();
    assert_eq!(cgroup_headroom_mb(&four_gib, &one_gib), Some(3072));
}

#[test]
fn cgroup_headroom_saturates_when_over_limit() {
    // A transient current > max must yield 0, not an underflow panic.
    assert_eq!(cgroup_headroom_mb("1024", "999999999"), Some(0));
}

#[test]
fn cgroup_headroom_rejects_garbage() {
    assert_eq!(cgroup_headroom_mb("not-a-number", "123"), None);
}

fn passing_result_with_timings(total_ms: u64, execute_ms: u64) -> TestResult {
    TestResult {
        name: "test_budget".to_string(),
        file: "tests/test_budget.harn".to_string(),
        passed: true,
        error: None,
        captured_output: None,
        timeout: None,
        duration_ms: total_ms,
        phases: Some(PhaseTimings {
            setup_ms: 7,
            compile_ms: 3,
            execute_ms,
            teardown_ms: 2,
            modules: harn_vm::ModulePhaseStats::default(),
        }),
    }
}

#[test]
fn enforce_case_budgets_fails_slow_total_wall_time() {
    let result = passing_result_with_timings(1_250, 100);

    let result = enforce_case_budgets(result, Some(1_000), None);

    assert!(!result.passed);
    let error = result.error.unwrap_or_default();
    assert!(error.contains("exceeded test wall-clock budget: 1250ms > 1000ms"));
    assert!(error.contains("phase timings: setup=7ms compile=3ms execute=100ms"));
}

#[test]
fn enforce_case_budgets_fails_slow_execute_phase() {
    let result = passing_result_with_timings(900, 750);

    let result = enforce_case_budgets(result, Some(1_000), Some(500));

    assert!(!result.passed);
    let error = result.error.unwrap_or_default();
    assert!(error.contains("exceeded test execute budget: 750ms > 500ms"));
    assert!(!error.contains("exceeded test wall-clock budget"));
}

#[test]
fn enforce_case_budgets_preserves_existing_failure() {
    let mut result = passing_result_with_timings(2_000, 1_000);
    result.passed = false;
    result.error = Some("assertion failed".to_string());

    let result = enforce_case_budgets(result, Some(1), Some(1));

    assert!(!result.passed);
    assert_eq!(result.error.as_deref(), Some("assertion failed"));
}

#[test]
fn sort_cases_longest_first_uses_historical_durations() {
    let source = Arc::new(String::new());
    let program = Arc::new(Vec::new());
    let mk = |name: &str| TestCase {
        file: PathBuf::from("tests/a.harn"),
        name: name.to_string(),
        pipeline_name: name.to_string(),
        source: Arc::clone(&source),
        program: Arc::clone(&program),
        imported_enum_candidates: Arc::new(Vec::new()),
        serial_group: None,
        weight: 1,
        bindings: Vec::new(),
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
fn test_shard_validation_rejects_invalid_selection() {
    assert!(TestShard::new(1, 1).is_ok());
    assert!(TestShard::new(0, 2).is_err());
    assert!(TestShard::new(1, 0).is_err());
    assert!(TestShard::new(3, 2).is_err());
}

#[test]
fn select_shard_cases_balances_by_historical_duration() {
    let source = Arc::new(String::new());
    let program = Arc::new(Vec::new());
    let mk = |name: &str| TestCase {
        file: PathBuf::from("tests/a.harn"),
        name: name.to_string(),
        pipeline_name: name.to_string(),
        source: Arc::clone(&source),
        program: Arc::clone(&program),
        imported_enum_candidates: Arc::new(Vec::new()),
        serial_group: None,
        weight: 1,
        bindings: Vec::new(),
    };
    let mut timings = BTreeMap::new();
    timings.insert("tests/a.harn::test_big".to_string(), 100);
    timings.insert("tests/a.harn::test_mid".to_string(), 60);
    timings.insert("tests/a.harn::test_small_a".to_string(), 40);
    timings.insert("tests/a.harn::test_small_b".to_string(), 20);

    let cases = vec![
        mk("test_big"),
        mk("test_mid"),
        mk("test_small_a"),
        mk("test_small_b"),
    ];
    let shard_one = select_shard_cases(cases.clone(), &timings, TestShard::new(1, 2).unwrap());
    let shard_two = select_shard_cases(cases, &timings, TestShard::new(2, 2).unwrap());

    let names_one = shard_one
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    let names_two = shard_two
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names_one, vec!["test_big", "test_small_b"]);
    assert_eq!(names_two, vec!["test_mid", "test_small_a"]);
}

#[test]
fn resource_gate_serializes_same_group() {
    // Deterministic, in-process: while one permit for a group is held, a
    // second acquire for the SAME group cannot proceed; releasing frees it.
    // (Previously this used two threads + `thread::sleep` to coax an
    // ordering, which was both flaky and ~60ms of wall-clock per run.)
    let gate = ResourceGate::new(4);
    let g_a = gate.acquire(1, Some("login"));
    assert!(
        gate.try_acquire(1, Some("login")).is_none(),
        "second acquire of a busy group must not proceed",
    );
    drop(g_a);
    assert!(
        gate.try_acquire(1, Some("login")).is_some(),
        "group should be free once the holder releases",
    );
}

#[test]
fn resource_gate_allows_independent_groups_in_parallel() {
    // Holding one group must never block an unrelated group, as long as
    // permits remain. No threads needed — `try_acquire` proves it directly.
    let gate = ResourceGate::new(4);
    let _guard_a = gate.acquire(1, Some("alpha"));
    assert!(
        gate.try_acquire(1, Some("beta")).is_some(),
        "an unrelated group must acquire without blocking",
    );
}

#[test]
fn resource_gate_caps_heavy_weight_at_capacity() {
    // A test that asks for more than the pool size must still be
    // schedulable (weight is capped to capacity) rather than deadlocking,
    // and while it holds the whole pool no other task can acquire.
    let gate = ResourceGate::new(2);
    let g = gate.acquire(99, None);
    assert!(
        gate.try_acquire(1, None).is_none(),
        "pool is fully consumed; a single-weight task must wait",
    );
    drop(g);
    assert!(
        gate.try_acquire(1, None).is_some(),
        "permit becomes available once the heavy holder releases",
    );
}

#[tokio::test]
async fn parallel_scheduler_runs_heavy_tests_without_oversubscribing() {
    // Heavy(2) should never run concurrently with another test when
    // the pool only has two workers — there are no spare permits.
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_heavy.harn",
        r"
@test
@heavy(threads: 2)
pipeline test_heavy_one(task) {}

@test
pipeline test_light(task) {}
",
    );

    let opts = RunOptions {
        parallel: true,
        jobs: Some(2),
        ..RunOptions::new(5_000)
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
        parallel: true,
        jobs: Some(4),
        ..RunOptions::new(5_000)
    };
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
    assert_eq!(summary.failed, 0, "{:?}", summary.results);
    assert_eq!(summary.passed, 2);
}

#[tokio::test]
async fn parallel_pipelines_isolate_egress_policy_and_http_mocks() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let _egress_env = [
        harn_vm::egress::HARN_EGRESS_ALLOW_ENV,
        harn_vm::egress::HARN_EGRESS_DENY_ENV,
        harn_vm::egress::HARN_EGRESS_DEFAULT_ENV,
        harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV,
        harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV,
    ]
    .map(ScopedEnvVar::unset);
    let temp = TempTestDir::new();
    let source = (0..32)
        .map(|index| {
            format!(
                r#"
pipeline test_policy_{index}(_task) {{
  const url = "https://case-{index}.example.test/data"
  egress_policy({{default: "deny", allow: ["case-{index}.example.test"]}})
  http_mock("GET", url, {{status: 200, body: "case-{index}", headers: {{}}}})
  const response = http_get(url)
  assert_eq(response.body, "case-{index}")
}}
"#
            )
        })
        .collect::<String>();
    temp.write("suite/test_egress_parallel.harn", &source);

    let opts = RunOptions {
        parallel: true,
        jobs: Some(8),
        ..RunOptions::new(5_000)
    };
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(
        summary.failed,
        0,
        "parallel egress state leaked: {:?}",
        summary
            .results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| (&result.name, &result.error))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.passed, 32);
}

#[tokio::test]
async fn environment_egress_policy_precedes_each_pipeline_policy() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let _allow = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_ENV);
    let _deny = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DENY_ENV);
    let _default = ScopedEnvVar::set(harn_vm::egress::HARN_EGRESS_DEFAULT_ENV, "deny");
    let _block_private = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV);
    let _allow_loopback = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_egress_environment.harn",
        r#"
pipeline test_environment_one(_task) {
  egress_policy({default: "allow"})
}

pipeline test_environment_two(_task) {
  egress_policy({default: "allow"})
}
"#,
    );
    let opts = RunOptions {
        parallel: true,
        jobs: Some(2),
        ..RunOptions::new(5_000)
    };

    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(summary.failed, 2);
    assert!(summary.results.iter().all(|result| {
        result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("policy already configured from environment"))
    }));
}

#[tokio::test]
async fn parallel_scheduler_persists_timings_cache() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_timed.harn",
        r"
@test
pipeline test_first(task) {}

@test
pipeline test_second(task) {}
",
    );

    let opts = RunOptions {
        parallel: true,
        jobs: Some(2),
        ..RunOptions::new(5_000)
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

/// Regression fixture: a worker thread that runs multiple cases must
/// reset thread-local state between them. Test A pins the clock
/// mock to a future timestamp; test B asserts the clock is fresh.
/// Fails if the per-case `reset_thread_local_state()` in
/// `execute_case` ever regresses. Pins workers to 1 so both tests
/// land on the same scheduler thread.
#[tokio::test]
async fn worker_resets_thread_local_state_between_cases() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_isolation.harn",
        r#"
// The leak probe pins the clock to a future-but-i64-safe value
// (year ~2128) so a leaked mock is observable. Larger values overflow
// the nanosecond conversion inside the mock clock.
pipeline test_a_pins_clock(task) {
  mock_time(5000000000000)
  assert_eq(now_ms(), 5000000000000)
}

pipeline test_b_clock_is_fresh(task) {
  const ms = now_ms()
  assert(ms < 5000000000000, "clock mock leaked from previous test")
}
"#,
    );

    let opts = RunOptions::new(5_000);
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
    assert_eq!(
        summary.failed,
        0,
        "state leaked between tests: {:?}",
        summary
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| (r.name.clone(), r.error.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.passed, 2);
}

// The synchronous guard serializes process-global state that Harn worker threads
// consult while this async test runs; dropping it before the runner completes
// would reintroduce the cross-test state race this fixture covers.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn user_tests_isolate_persistent_runtime_state_per_case() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
    let ambient_state = tempfile::tempdir().expect("ambient state tempdir");
    let _ambient_state_guard = ScopedEnvVar::set(
        harn_vm::runtime_paths::HARN_STATE_DIR_ENV,
        ambient_state.path().to_string_lossy().as_ref(),
    );

    for parallel in [false, true] {
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_store_isolation.harn",
            r#"
pipeline test_a_sets_store_value(task) {
  store_set("test-only-key", "from-a")
  assert_eq(store_get("test-only-key"), "from-a")
  metadata_set(".", "test", {value: "from-a"})
  metadata_save()
  assert_eq(metadata_get(".", "test").value, "from-a")
  checkpoint("test-only-key", "from-a")
  assert_eq(checkpoint_get("test-only-key"), "from-a")
}

pipeline test_b_has_fresh_store(task) {
  assert_eq(store_get("test-only-key"), nil)
  assert_eq(metadata_get(".", "test"), nil)
  assert_eq(checkpoint_get("test-only-key"), nil)
}
"#,
        );

        let opts = RunOptions {
            parallel,
            jobs: parallel.then_some(2),
            ..RunOptions::new(5_000)
        };
        let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;
        assert_eq!(
            summary.failed,
            0,
            "persistent state leaked with parallel={parallel}: {:?}",
            summary
                .results
                .iter()
                .filter(|result| !result.passed)
                .map(|result| (result.name.clone(), result.error.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(summary.passed, 2);
        assert!(
            !temp.path().join("store.json").exists(),
            "user tests must not write persistent state into the project root"
        );
    }

    assert!(
        !ambient_state.path().join("store.json").exists(),
        "user tests must not write stores to the ambient runtime state root"
    );
    assert!(
        !ambient_state.path().join("metadata").exists(),
        "user tests must not write metadata to the ambient runtime state root"
    );
    assert!(
        !ambient_state.path().join("checkpoints").exists(),
        "user tests must not write checkpoints to the ambient runtime state root"
    );
}

#[cfg(feature = "hostlib")]
#[tokio::test]
async fn user_tests_scope_conditional_replacement_locks_to_case_state() {
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_safe_text_patch.harn",
        r#"
import { with_temp_dir } from "std/testing"

pipeline test_safe_text_patch_uses_case_state(task) {
  const _ = hostlib_enable("tools:deterministic")
  with_temp_dir(
    { root ->
      const path = root + "/notes.txt"
      harness.fs.write_text(path, "before\n")
      const applied = hostlib_fs_safe_text_patch({path: path, content: "after\n"})
      assert_eq(applied.result, "applied")
      const stale = hostlib_fs_safe_text_patch(
        {
          path: path,
          content: "stale\n",
          expected_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        },
      )
      assert_eq(stale.result, "stale_base")
      assert_eq(harness.fs.read_text(path), "after\n")
    },
  )
}
"#,
    );

    let summary = run_tests(&temp.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(
        summary.passed, 1,
        "safe-text-patch user test did not pass: {:?}",
        summary.results
    );
    assert_eq!(
        summary.failed, 0,
        "safe-text-patch user test failed: {:?}",
        summary.results
    );
}

#[tokio::test]
async fn summary_aggregate_timings_sum_phases_across_results() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_phases.harn",
        r"
pipeline test_one(task) { assert_eq(1, 1) }
pipeline test_two(task) { assert_eq(2, 2) }
",
    );

    let summary = run_tests(&temp.path().join("suite"), None, 5_000, false, &[]).await;
    assert_eq!(summary.passed, 2);
    let per_test_sum: u64 = summary
        .results
        .iter()
        .filter_map(|result| result.phases)
        .map(|phases| phases.setup_ms.saturating_add(phases.compile_ms))
        .sum();
    let agg_sum = summary
        .aggregate
        .setup_ms
        .saturating_add(summary.aggregate.compile_ms);
    assert_eq!(
        per_test_sum, agg_sum,
        "aggregate setup+compile must equal sum of per-test setup+compile"
    );
}

#[tokio::test]
async fn parallel_scheduler_emits_progress_events() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_events.harn",
        r"
@test
pipeline test_a(task) {}

@test
pipeline test_b(task) {}
",
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
        parallel: true,
        jobs: Some(2),
        progress: Some(progress),
        ..RunOptions::new(5_000)
    };
    let _ = run_tests_with_options(&temp.path().join("suite"), &opts).await;
    let events = events.lock().unwrap();
    assert_eq!(events.first().copied(), Some("suite"));
    assert_eq!(events.iter().filter(|e| **e == "started").count(), 2);
    assert_eq!(events.iter().filter(|e| **e == "finished").count(), 2);
}

#[tokio::test]
async fn fail_fast_stops_sequential_execution_after_first_failure() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_fail_fast.harn",
        r#"
pipeline test_a_fails(task) { assert(false, "first failure") }
pipeline test_z_must_not_run(task) { assert(false, "second case ran") }
"#,
    );

    let opts = RunOptions {
        fail_fast: true,
        ..RunOptions::new(5_000)
    };
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(summary.total, 1, "only the first case should execute");
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.results[0].name, "test_a_fails");
}

#[tokio::test]
async fn fail_fast_discovery_error_prevents_case_execution() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write("suite/test_broken.harn", "pipeline test_broken( {");
    temp.write(
        "suite/test_valid.harn",
        "pipeline test_valid(task) { assert(false, \"case ran\") }",
    );

    let opts = RunOptions {
        fail_fast: true,
        ..RunOptions::new(5_000)
    };
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(summary.total, 1);
    assert_eq!(summary.results[0].name, "<file error>");
}

#[test]
fn fail_fast_parallel_claim_refuses_queued_case_after_cancellation() {
    let source = Arc::new("pipeline test_one(task) {}".to_string());
    let program = Arc::new(parse_program(&source).unwrap());
    let cases =
        extract_cases_from_program(Path::new("test_one.harn"), &source, &program, None, 2).unwrap();
    let queue = Mutex::new(cases);
    let cancelled = AtomicBool::new(true);

    assert!(claim_next_case(&queue, &cancelled, true).is_none());
    assert_eq!(queue.lock().unwrap().len(), 1, "case must remain unclaimed");
    assert!(claim_next_case(&queue, &cancelled, false).is_some());
}

#[tokio::test]
async fn parameterized_test_rows_bind_values_and_report_independently() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_parameterized.harn",
        r#"
@test(cases: [
  {name: "passes", args: [2, 2]},
  {name: "fails", args: [2, 3]},
  {name: "also_passes", args: [4, 4]},
])
pipeline test_equal(actual, expected) {
  assert_eq(actual, expected)
}
"#,
    );

    let summary = run_tests(&temp.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.total, 3);
    assert_eq!(summary.passed, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary
            .results
            .iter()
            .map(|result| result.name.as_str())
            .collect::<Vec<_>>(),
        [
            "test_equal[also_passes]",
            "test_equal[fails]",
            "test_equal[passes]"
        ]
    );
}

#[tokio::test]
async fn parameterized_test_filter_selects_individual_row() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_parameterized.harn",
        r#"
@test(cases: [
  {name: "ascii", args: ["abc", 3]},
  {name: "empty", args: ["", 0]},
])
pipeline test_length(value, expected) { assert_eq(len(value), expected) }
"#,
    );

    let summary = run_tests(
        &temp.path().join("suite"),
        Some("[empty]"),
        5_000,
        false,
        &[],
    )
    .await;

    assert_eq!(summary.total, 1);
    assert_eq!(summary.passed, 1);
    assert_eq!(summary.results[0].name, "test_length[empty]");
}

#[tokio::test]
async fn malformed_parameterized_rows_fail_during_discovery() {
    let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_parameterized.harn",
        r#"
@test(cases: [
  {name: "duplicate", args: [1]},
  {name: "duplicate", args: [2]},
])
pipeline test_value(value) { assert(false, "must not execute") }
"#,
    );

    let summary = run_tests(&temp.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.total, 1);
    assert_eq!(summary.results[0].name, "<file error>");
    assert_eq!(summary.timing.sample_count, 0);
    assert!(summary.results[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("non-empty and unique")));
}
