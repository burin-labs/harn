use super::*;

use std::sync::Arc;

use harn_vm::VmValue;

mod compile_once_tests;
mod sharding;

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
fn affected_test_selection_follows_transitive_module_importers() {
    let temp = TempTestDir::new();
    temp.write(
        "suite/lib/shared.harn",
        "pub fn value() -> int { return 1 }",
    );
    temp.write(
        "suite/test_direct.harn",
        "import { value } from \"lib/shared.harn\"\npipeline test_direct(_task) { assert_eq(value(), 1) }",
    );
    temp.write(
        "suite/test_unrelated.harn",
        "pipeline test_unrelated(_task) { assert(true) }",
    );

    let selection = select_affected_test_files(
        &[temp.path().join("suite")],
        &[temp.path().join("suite/lib/shared.harn")],
    );
    assert_eq!(
        selection,
        AffectedTestFiles::Selected {
            files: vec![temp
                .path()
                .join("suite/test_direct.harn")
                .canonicalize()
                .unwrap()]
        }
    );
}

#[test]
fn affected_test_selection_falls_back_for_unmodelled_harn_files() {
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_known.harn",
        "pipeline test_known(_task) { assert(true) }",
    );
    temp.write("dynamic_helper.harn", "pub fn helper() -> int { return 1 }");

    let selection = select_affected_test_files(
        &[temp.path().join("suite")],
        &[temp.path().join("dynamic_helper.harn")],
    );
    match selection {
        AffectedTestFiles::Full { files, reason } => {
            assert!(reason.contains("outside the resolved test module graph"));
            assert_eq!(
                files,
                vec![temp
                    .path()
                    .join("suite/test_known.harn")
                    .canonicalize()
                    .unwrap()]
            );
        }
        other => panic!("expected complete-suite fallback, got {other:?}"),
    }
}

#[tokio::test]
async fn curated_paths_share_one_suite_and_deduplicate_overlaps() {
    let temp = TempTestDir::new();
    temp.write("suite/test_alpha.harn", "pipeline test_alpha(_task) {}");
    temp.write(
        "suite/nested/test_beta.harn",
        "pipeline test_beta(_task) {}",
    );
    let paths = vec![
        temp.path().join("suite/test_alpha.harn"),
        temp.path().join("suite/nested"),
        temp.path().join("suite/nested/test_beta.harn"),
    ];
    let options = RunOptions::new(30_000);
    let summary =
        run_tests_with_paths_and_operator_grant(&paths, &options, &TestRunSession::default(), None)
            .await;

    assert_eq!(summary.total, 2);
    assert_eq!(summary.passed, 2);
    assert_eq!(summary.failed, 0);
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
        args: vec![VmValue::Nil],
        fixture: None,
        file_fixture_value: None,
        compiled_entry: None,
        compiled_file_fixture_entry: None,
        trusted_host_dispatch: false,
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
        None,
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
        args: vec![VmValue::Nil],
        fixture: None,
        file_fixture_value: None,
        compiled_entry: None,
        compiled_file_fixture_entry: None,
        trusted_host_dispatch: false,
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
        None,
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
        "pipeline test_probe(harness: Harness, _task) { harness.stdio.log(\"HELLO_PROBE\"); return 1 }",
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
async fn execute_case_receipt_includes_named_std_timing_spans() {
    let temp = TempTestDir::new();
    let result = run_single_case(
        &temp,
        "test_timed_suboperation",
        r#"
import { timed } from "std/timing"

pipeline test_timed_suboperation(harness: Harness, _task) {
  const measured = timed(harness.clock, "property.sweep.case_17", {case_id: 17}, { -> 42 })
  assert_eq(measured.result, 42)
}
"#,
    )
    .await;

    assert!(result.passed, "case should pass: {:?}", result.error);
    let span = result
        .timing_spans
        .iter()
        .find(|span| span.name == "property.sweep.case_17")
        .expect("the test receipt must retain the script-owned timing span");
    assert_eq!(span.attributes.get("case_id"), Some(&serde_json::json!(17)));
}

#[tokio::test]
async fn execute_case_captures_log_output_for_a_failing_case() {
    let temp = TempTestDir::new();
    let result = run_single_case(
        &temp,
        "test_probe_fail",
        "pipeline test_probe_fail(harness: Harness, _task) { harness.stdio.log(\"HELLO_FAIL_PROBE\"); assert(false, \"boom\") }",
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
        args: vec![VmValue::Nil],
        fixture: None,
        file_fixture_value: None,
        compiled_entry: None,
        compiled_file_fixture_entry: None,
        trusted_host_dispatch: false,
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
        None,
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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

#[tokio::test]
async fn cold_import_graph_is_compiled_once_before_parallel_case_budgets_start() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    let cache_dir = temp.path().join("bytecode-cache");
    let _cache_dir = ScopedEnvVar::set(
        harn_vm::bytecode_cache::CACHE_DIR_ENV,
        &cache_dir.to_string_lossy(),
    );
    let _cache_enabled = ScopedEnvVar::set(harn_vm::bytecode_cache::CACHE_ENABLED_ENV, "1");
    for index in (0..16).rev() {
        let source = if index == 15 {
            "pub fn value_15() { return 255 }\n".to_string()
        } else {
            format!(
                "import {{ value_{} }} from \"./module_{}\"\npub fn value_{index}() {{ return value_{}() }}\n",
                index + 1,
                index + 1,
                index + 1,
            )
        };
        temp.write(&format!("suite/module_{index}.harn"), &source);
    }
    temp.write(
        "suite/test_first.harn",
        r#"
import { value_0 } from "./module_0"
pipeline test_first(_task) { assert_eq(value_0(), 255) }
"#,
    );
    temp.write(
        "suite/test_second.harn",
        r#"
import { value_0 } from "./module_0"
pipeline test_second(_task) { assert_eq(value_0(), 255) }
"#,
    );
    let options = RunOptions {
        parallel: true,
        jobs: Some(2),
        ..RunOptions::new(5_000)
    };
    let session = TestRunSession::default();

    let summary = run_tests_with_session(&temp.path().join("suite"), &options, &session).await;

    assert_eq!(summary.passed, 2, "{:?}", summary.results);
    assert_eq!(
        summary.aggregate.modules.modules_compiled, 16,
        "the suite compile phase should prepare each transitive module exactly once"
    );
    assert!(
        summary
            .results
            .iter()
            .filter_map(|result| result.phases)
            .all(|phases| phases.modules.modules_compiled == 0),
        "no individual test execution may be billed for cold module compilation: {:?}",
        summary.results
    );
    assert_eq!(
        session.stats().workers,
        2,
        "both workers must consume the one shared prepared graph"
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_cwd.harn",
        r"
pipeline test_current_dir(harness: Harness, task) {
  assert_eq(harness.fs.cwd(), harness.fs.source_dir())
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/a/test_one.harn",
        r"
pipeline test_one(harness: Harness, task) {
  assert_eq(harness.fs.cwd(), harness.fs.source_dir())
}
",
    );
    temp.write(
        "suite/b/test_two.harn",
        r"
pipeline test_two(harness: Harness, task) {
  assert_eq(harness.fs.cwd(), harness.fs.source_dir())
}
",
    );

    let summary = run_tests(&temp.path().join("suite"), None, 1_000, true, &[]).await;
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.passed, 2);
}

#[tokio::test]
async fn run_tests_loads_cli_skill_dirs() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _backend_guard = ScopedEnvVar::unset(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV);
    let _dir_guard = ScopedEnvVar::unset(harn_vm::event_log::HARN_EVENT_LOG_DIR_ENV);
    let _sqlite_guard = ScopedEnvVar::unset(harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV);
    let _state_guard = ScopedEnvVar::unset(harn_vm::runtime_paths::HARN_STATE_DIR_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_store.harn",
        r#"
pipeline test_store_builtin_uses_runner_event_log(harness: Harness, task) {
  harness.runtime.store_set("test.key", "value")
  assert_eq(harness.runtime.store_get("test.key"), "value")
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
fn timing_budgets_own_a_serial_measurement_lane() {
    let mut opts = RunOptions::new(1_000);
    opts.parallel = true;
    opts.jobs = Some(8);
    opts.max_execute_ms = Some(500);
    assert_eq!(resolve_workers(&opts), 1);

    opts.max_execute_ms = None;
    opts.max_test_ms = Some(750);
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
            admission_ms: 0,
            execute_ms,
            teardown_ms: 2,
            modules: harn_vm::ModulePhaseStats::default(),
        }),
        timing_spans: Vec::new(),
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
fn process_admission_wait_is_projected_out_of_user_execution() {
    let result = attribute_admission_wait(passing_result_with_timings(120, 100), 35);
    let phases = result.phases.expect("passing result has phase timings");

    assert_eq!(phases.admission_ms, 35);
    assert_eq!(phases.execute_ms, 65);
    assert_eq!(result.duration_ms, 120, "wall time remains factual");
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
        args: Vec::new(),
        fixture: None,
        file_fixture_value: None,
        compiled_entry: None,
        compiled_file_fixture_entry: None,
        trusted_host_dispatch: false,
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

#[tokio::test]
async fn parallel_scheduler_runs_heavy_tests_without_oversubscribing() {
    // Heavy(2) should never run concurrently with another test when
    // the pool only has two workers — there are no spare permits.
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
pipeline test_policy_{index}(harness: Harness, _task) {{
  const url = "https://case-{index}.example.test/data"
  harness.net.egress_policy({{default: "deny", allow: ["case-{index}.example.test"]}})
  harness.testing.http_mock("GET", url, {{status: 200, body: "case-{index}", headers: {{}}}})
  const response = harness.net.get(url)
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _allow = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_ENV);
    let _deny = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DENY_ENV);
    let _default = ScopedEnvVar::set(harn_vm::egress::HARN_EGRESS_DEFAULT_ENV, "deny");
    let _block_private = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV);
    let _allow_loopback = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_egress_environment.harn",
        r#"
pipeline test_environment_one(harness: Harness, _task) {
  harness.net.egress_policy({default: "allow"})
}

pipeline test_environment_two(harness: Harness, _task) {
  harness.net.egress_policy({default: "allow"})
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

/// Regression fixture: a worker thread that runs multiple cases must
/// reset thread-local state between them. Test A pins the clock
/// mock to a future timestamp; test B asserts the clock is fresh.
/// Fails if the per-case `reset_thread_local_state()` in
/// `execute_case` ever regresses. Pins workers to 1 so both tests
/// land on the same scheduler thread.
#[tokio::test]
async fn worker_resets_thread_local_state_between_cases() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_isolation.harn",
        r#"
// The leak probe pins the clock to a future-but-i64-safe value
// (year ~2128) so a leaked mock is observable. Larger values overflow
// the nanosecond conversion inside the mock clock.
pipeline test_a_pins_clock(harness: Harness, task) {
  harness.testing.clock_set(5000000000000)
  assert_eq(harness.clock.now_ms(), 5000000000000)
}

pipeline test_b_clock_is_fresh(harness: Harness, task) {
  const ms = harness.clock.now_ms()
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

// The state guard serializes process-global state that Harn worker threads
// consult while this async test runs; dropping it before the runner completes
// would reintroduce the cross-test state race this fixture covers.
#[tokio::test(flavor = "current_thread")]
async fn user_tests_isolate_persistent_runtime_state_per_case() {
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
pipeline test_a_sets_store_value(harness: Harness, task) {
  harness.runtime.store_set("test-only-key", "from-a")
  assert_eq(harness.runtime.store_get("test-only-key"), "from-a")
  harness.project.metadata_set({dir: ".", namespace: "test", value: {value: "from-a"}})
  harness.project.metadata_save({})
  assert_eq(harness.project.metadata_get({dir: ".", namespace: "test"}).value, "from-a")
  harness.runtime.checkpoint("test-only-key", "from-a")
  assert_eq(harness.runtime.checkpoint_get("test-only-key"), "from-a")
  harness.agent.session_store_append("test-only-session", {case: "from-a"})
  assert_eq(len(harness.agent.session_store_events("test-only-session").value), 1)
}

pipeline test_b_has_fresh_store(harness: Harness, task) {
  assert_eq(harness.runtime.store_get("test-only-key"), nil)
  assert_eq(harness.project.metadata_get({dir: ".", namespace: "test"}), nil)
  assert_eq(harness.runtime.checkpoint_get("test-only-key"), nil)
  harness.agent.session_store_append("test-only-session", {case: "from-b"})
  assert_eq(len(harness.agent.session_store_events("test-only-session").value), 1)
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
    assert!(
        !ambient_state.path().join("session-store.sqlite").exists(),
        "user tests must not write agent sessions to the ambient runtime state root"
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

pipeline test_safe_text_patch_uses_case_state(harness: Harness, task) {
  with_temp_dir(
    harness.fs,
    { root ->
      const path = root + "/notes.txt"
      harness.fs.write_text(path, "before\n")
      const applied = harness.fs.safe_text_patch({path: path, content: "after\n"})
      assert_eq(applied.result, "applied")
      const stale = harness.fs.safe_text_patch(
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
async fn summary_aggregate_timings_include_suite_preparation_and_case_phases() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let per_test_setup: u64 = summary
        .results
        .iter()
        .filter_map(|result| result.phases)
        .map(|phases| phases.setup_ms)
        .sum();
    assert_eq!(
        per_test_setup, summary.aggregate.setup_ms,
        "aggregate setup must equal the sum of per-test setup"
    );
    let per_test_compile: u64 = summary
        .results
        .iter()
        .filter_map(|result| result.phases)
        .map(|phases| phases.compile_ms)
        .sum();
    assert!(
        summary.aggregate.compile_ms >= per_test_compile,
        "aggregate compile includes suite graph preparation before per-test compile: {:?}",
        summary.aggregate
    );
}

#[tokio::test]
async fn parallel_scheduler_emits_progress_events() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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

#[tokio::test]
async fn parameterized_test_rows_bind_values_and_report_independently() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
async fn parameterized_row_type_failure_does_not_block_siblings() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_typed_rows.harn",
        r#"
@test(cases: [
  {name: "wrong_type", args: ["not an integer"]},
  {name: "valid", args: [42]},
])
pipeline test_typed(value: int) {
  assert_eq(value, 42)
}
"#,
    );

    let summary = run_tests(&temp.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.total, 2, "{:?}", summary.results);
    assert_eq!(summary.passed, 1, "{:?}", summary.results);
    assert_eq!(summary.failed, 1, "{:?}", summary.results);
    let failure = summary
        .results
        .iter()
        .find(|result| result.name == "test_typed[wrong_type]")
        .expect("wrong-type row result");
    assert!(
        failure
            .error
            .as_deref()
            .is_some_and(|error| error.contains("expected int")),
        "row values must pass through ordinary callable type checks: {failure:?}"
    );
    assert!(summary
        .results
        .iter()
        .any(|result| result.name == "test_typed[valid]" && result.passed));
}

#[tokio::test]
async fn parameterized_test_filter_selects_individual_row() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
