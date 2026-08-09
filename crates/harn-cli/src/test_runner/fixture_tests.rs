use super::*;

use std::fs;

struct FixtureTestDir {
    inner: tempfile::TempDir,
}

impl FixtureTestDir {
    fn new() -> Self {
        Self {
            inner: tempfile::tempdir().unwrap(),
        }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.inner.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}

#[tokio::test]
async fn file_fixture_runs_once_and_cow_value_isolates_parameterized_cases() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = FixtureTestDir::new();
    temp.write(
        "suite/test_file_fixture.harn",
        r#"
@test_fixture(scope: file)
fn shared_fixture(harness: Harness) -> dict {
  harness.fs.append_locked("fixture-calls.log", "called\n")
  return {items: [1]}
}

@test(
  cases: [
    {name: "first", args: [10]},
    {name: "second", args: [20]},
  ],
  fixture: shared_fixture,
)
pipeline test_isolated(fx: dict, replacement: int) {
  assert_eq(fx.items[0], 1)
  let local = fx
  local.items[0] = replacement
  assert_eq(local.items[0], replacement)
}
"#,
    );

    let suite = temp.inner.path().join("suite");
    let summary = run_tests(&suite, None, 5_000, false, &[]).await;

    assert_eq!(summary.passed, 2, "{:?}", summary.results);
    assert_eq!(summary.failed, 0, "{:?}", summary.results);
    assert_eq!(summary.aggregate.test_files_compiled, 1);
    assert_eq!(
        summary.aggregate.test_entries_compiled, 2,
        "the file fixture and parameterized pipeline must share one file lowering"
    );
    assert_eq!(
        fs::read_to_string(suite.join("fixture-calls.log")).unwrap(),
        "called\n",
        "file fixture must execute exactly once for all selected rows"
    );
}

#[tokio::test]
async fn case_fixture_runs_inside_each_fresh_case_vm() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = FixtureTestDir::new();
    temp.write(
        "suite/test_case_fixture.harn",
        r#"
let calls = 0

@test_fixture(scope: case)
fn fresh_fixture() -> dict {
  calls = calls + 1
  return {calls: calls}
}

@test(
  cases: [
    {name: "first", args: ["a"]},
    {name: "second", args: ["b"]},
  ],
  fixture: fresh_fixture,
)
pipeline test_fresh(fx: dict, label: string) {
  assert_eq(fx.calls, 1)
  assert(len(label) > 0)
}
"#,
    );

    let summary = run_tests(&temp.inner.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.passed, 2, "{:?}", summary.results);
    assert_eq!(summary.failed, 0, "{:?}", summary.results);
}

#[tokio::test]
async fn file_fixture_failure_is_one_typed_result_and_suppresses_its_cases() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = FixtureTestDir::new();
    temp.write(
        "suite/test_fixture_failure.harn",
        r#"
@test_fixture(scope: file)
fn broken_fixture() -> int {
  assert(false, "fixture exploded")
  return 1
}

@test(
  cases: [
    {name: "first", args: []},
    {name: "second", args: []},
  ],
  fixture: broken_fixture,
)
pipeline test_suppressed(_fixture: int) {
  assert(false, "suppressed case executed")
}

pipeline test_unrelated() {}
"#,
    );

    let summary = run_tests(&temp.inner.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.total, 2, "{:?}", summary.results);
    assert_eq!(summary.passed, 1, "{:?}", summary.results);
    assert_eq!(summary.failed, 1, "{:?}", summary.results);
    assert_eq!(
        summary
            .results
            .iter()
            .filter(|result| result.name == "<fixture broken_fixture>")
            .count(),
        1
    );
    assert!(summary
        .results
        .iter()
        .all(|result| !result.name.starts_with("test_suppressed[")));
}

#[tokio::test]
async fn fail_fast_stops_before_later_file_fixture_setup() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = FixtureTestDir::new();
    temp.write(
        "suite/a_broken.harn",
        r#"
@test_fixture(scope: file)
fn broken() -> int {
  assert(false, "stop here")
  return 1
}

@test(fixture: broken)
pipeline test_broken(_fixture: int) {}
"#,
    );
    temp.write(
        "suite/z_later.harn",
        r#"
@test_fixture(scope: file)
fn later(harness: Harness) -> int {
  harness.fs.append_locked("later-fixture-ran.log", "unexpected\n")
  return 1
}

@test(fixture: later)
pipeline test_later(_fixture: int) {}
"#,
    );

    let suite = temp.inner.path().join("suite");
    let options = RunOptions {
        timeout_ms: 5_000,
        parallel: true,
        fail_fast: true,
        jobs: Some(2),
        ..RunOptions::default()
    };
    let summary = run_tests_with_options(&suite, &options).await;

    assert_eq!(summary.total, 1, "{:?}", summary.results);
    assert_eq!(summary.results[0].name, "<fixture broken>");
    assert!(
        !suite.join("later-fixture-ran.log").exists(),
        "fail-fast must stop claiming fixture setup work after the first setup failure"
    );
}

#[tokio::test]
async fn fixture_contract_errors_are_source_located_discovery_failures() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = FixtureTestDir::new();
    temp.write(
        "suite/test_bad_fixture.harn",
        r"
@test_fixture(scope: suite)
fn invalid_scope() -> int { return 1 }

@test(fixture: missing_fixture)
pipeline test_missing(_fixture: int) {}
",
    );

    let summary = run_tests(&temp.inner.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.total, 1);
    let error = summary.results[0].error.as_deref().unwrap();
    assert!(
        error.starts_with("2:"),
        "diagnostic must begin with source line and column: {error}"
    );
    assert!(error.contains("must be `file` or `case`"), "{error}");
}

#[tokio::test]
async fn malformed_parameterized_rows_are_source_located_discovery_failures() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let temp = FixtureTestDir::new();
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

    let summary = run_tests(&temp.inner.path().join("suite"), None, 5_000, false, &[]).await;

    assert_eq!(summary.total, 1);
    assert_eq!(summary.results[0].name, "<file error>");
    assert_eq!(summary.timing.sample_count, 0);
    let error = summary.results[0].error.as_deref().unwrap();
    assert!(error.starts_with("4:"), "{error}");
    assert!(error.contains("non-empty and unique"), "{error}");
}
