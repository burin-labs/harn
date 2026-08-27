//! End-to-end coverage for `harn test conformance --json`.

use std::process::{Command, Output};

use harn_cli::tests::common::json_envelope::assert_envelope;
use serde_json::Value;

const CONFORMANCE_TEST_SCHEMA_VERSION: u32 = 3;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn write_fixture(root: &std::path::Path, name: &str, source: &str, expected: &str) {
    let path = root.join("conformance").join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    std::fs::write(&path, source).expect("write source");
    std::fs::write(path.with_extension("expected"), expected).expect("write expected");
}

/// A `.harn` file with no expectation sibling: the runner can see it but
/// cannot run it. This is the shape that produced the original defect.
fn write_source_without_expectation(root: &std::path::Path, name: &str, source: &str) {
    let path = root.join("conformance").join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    std::fs::write(&path, source).expect("write source");
}

fn run_conformance(root: &std::path::Path, extra: &[&str]) -> Output {
    let mut args = vec!["test", "conformance", "--timeout", "10000"];
    args.extend_from_slice(extra);
    Command::new(binary_path())
        .args(&args)
        .current_dir(root)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn test conformance")
}

fn run_conformance_json(root: &std::path::Path) -> Output {
    Command::new(binary_path())
        .args(["test", "conformance", "--json", "--timeout", "10000"])
        .current_dir(root)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn test conformance --json")
}

fn run_parallel_conformance_json(root: &std::path::Path) -> Output {
    Command::new(binary_path())
        .args([
            "test",
            "conformance",
            "--json",
            "--parallel",
            "--jobs",
            "2",
            "--timeout",
            "10000",
        ])
        .current_dir(root)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn parallel harn test conformance --json")
}

fn parse_stdout(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not valid JSON ({error}):\n{stdout}"))
}

fn result<'a>(data: &'a Value, name: &str) -> &'a Value {
    data["results"]
        .as_array()
        .expect("results array")
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing result {name}: {}", data["results"]))
}

#[test]
fn conformance_json_reports_pass_and_expected_xfail() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(
        temp.path(),
        "pass.harn",
        "pipeline test(harness: Harness, task: unknown) {\n  harness.stdio.log(\"pass\")\n}\n",
        "[harn] pass\n",
    );
    write_fixture(
        temp.path(),
        "xfail_expected.harn",
        "// @xfail: tracked in #999\npipeline test(harness: Harness, task: unknown) {\n  harness.stdio.log(\"actual\")\n}\n",
        "[harn] expected\n",
    );

    let output = run_conformance_json(temp.path());
    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = parse_stdout(&output);
    let data = assert_envelope(&parsed, CONFORMANCE_TEST_SCHEMA_VERSION);
    assert_eq!(data["summary"]["pass"], 1);
    assert_eq!(data["summary"]["fail"], 0);
    assert_eq!(data["summary"]["xfail_expected"], 1);
    assert_eq!(data["summary"]["xfail_unexpected_pass"], 0);
    assert_eq!(data["timing"]["sample_count"], 2);
    assert!(data["timing"]["p90_ms"].as_u64().is_some());
    assert_eq!(result(data, "pass.harn")["outcome"], "pass");
    assert_eq!(
        result(data, "xfail_expected.harn")["outcome"],
        "xfail_expected"
    );
    let snapshot = data["snapshotKey"].as_str().expect("snapshotKey string");
    assert_eq!(snapshot.len(), 64);
    assert!(snapshot.chars().all(|ch| ch.is_ascii_hexdigit()));

    let second = run_conformance_json(temp.path());
    assert!(second.status.success());
    let second_parsed = parse_stdout(&second);
    let second_data = assert_envelope(&second_parsed, CONFORMANCE_TEST_SCHEMA_VERSION);
    assert_eq!(second_data["snapshotKey"], data["snapshotKey"]);
}

#[test]
fn parallel_conformance_json_matches_sequential_results() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    for index in 0..6 {
        write_fixture(
            temp.path(),
            &format!("group/case-{index}.harn"),
            &format!(
                "pipeline test(harness: Harness, task: unknown) {{\n  harness.stdio.println({index})\n}}\n"
            ),
            &format!("{index}\n"),
        );
    }

    let sequential = run_conformance_json(temp.path());
    let parallel = run_parallel_conformance_json(temp.path());
    assert!(
        sequential.status.success() && parallel.status.success(),
        "sequential stderr={}\nparallel stderr={}",
        String::from_utf8_lossy(&sequential.stderr),
        String::from_utf8_lossy(&parallel.stderr),
    );
    let sequential = parse_stdout(&sequential);
    let parallel = parse_stdout(&parallel);
    let sequential_data = assert_envelope(&sequential, CONFORMANCE_TEST_SCHEMA_VERSION);
    let parallel_data = assert_envelope(&parallel, CONFORMANCE_TEST_SCHEMA_VERSION);

    assert_eq!(parallel_data["snapshotKey"], sequential_data["snapshotKey"]);
    assert_eq!(parallel_data["summary"], sequential_data["summary"]);
    let stable_results = |data: &Value| {
        data["results"]
            .as_array()
            .expect("results array")
            .iter()
            .map(|result| {
                serde_json::json!({
                    "name": result["name"],
                    "outcome": result["outcome"],
                    "message": result["message"],
                    "diagnostic_codes": result["diagnostic_codes"],
                })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stable_results(parallel_data),
        stable_results(sequential_data)
    );
}

#[test]
fn parallel_text_conformance_skips_xfail_cases_without_executing_them() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(
        temp.path(),
        "xfail.harn",
        "// @xfail: tracked in #999\npipeline test(harness: Harness, task: unknown) {\n  harness.stdio.println(true)\n}\n",
        "true\n",
    );

    let output = Command::new(binary_path())
        .args([
            "test",
            "conformance",
            "--parallel",
            "--jobs",
            "2",
            "--timeout",
            "10000",
        ])
        .current_dir(temp.path())
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn parallel text conformance runner");

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("xfail.harn  (tracked in #999)"));
    assert!(stdout.contains("0 passed, 0 failed, 1 skipped, 1 total"));
}

#[test]
fn conformance_json_fails_on_failures_and_unexpected_xfail_passes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(
        temp.path(),
        "fail.harn",
        "pipeline test(harness: Harness, task: unknown) {\n  harness.stdio.log(\"actual\")\n}\n",
        "[harn] expected\n",
    );
    write_fixture(
        temp.path(),
        "xfail_unexpected_pass.harn",
        "// @xfail: tracked in #999\npipeline test(harness: Harness, task: unknown) {\n  harness.stdio.log(\"fixed\")\n}\n",
        "[harn] fixed\n",
    );

    let output = run_conformance_json(temp.path());
    assert!(
        !output.status.success(),
        "unexpected pass stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let parsed = parse_stdout(&output);
    let data = assert_envelope(&parsed, CONFORMANCE_TEST_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "conformance_failed");
    assert_eq!(data["summary"]["fail"], 1);
    assert_eq!(data["summary"]["xfail_unexpected_pass"], 1);
    assert_eq!(result(data, "fail.harn")["outcome"], "fail");
    assert_eq!(
        result(data, "xfail_unexpected_pass.harn")["outcome"],
        "xfail_unexpected_pass"
    );
}

#[test]
fn conformance_json_ignores_fixture_parent_runtime_state() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(
        temp.path(),
        "metadata/isolated.harn",
        "pipeline test(harness: Harness, task: unknown) {\n  assert_eq(harness.project.metadata_get({dir: \".\", namespace: \"classification\"}), nil)\n  const stale = harness.project.metadata_stale({dir: \".\"})\n  assert_eq(stale.any_stale, false)\n  const paths = harness.fs.runtime_paths()\n  assert_eq(paths.state_root.ends_with(\".harn\"), true)\n  assert_eq(paths.worktree_root.ends_with(\".harn/worktrees\"), true)\n  harness.stdio.log(\"isolated\")\n}\n",
        "[harn] isolated\n",
    );
    let stale_state = temp
        .path()
        .join("conformance")
        .join("metadata")
        .join(".harn")
        .join("metadata")
        .join("classification");
    std::fs::create_dir_all(&stale_state).expect("create stale metadata dir");
    std::fs::write(
        stale_state.join("entries.json"),
        r#"{
  "backend": "filesystem",
  "entries": {
    ".": {
      "structureHash": "stale"
    }
  },
  "namespace": "classification",
  "version": 1
}
"#,
    )
    .expect("write stale metadata");

    let output = run_conformance_json(temp.path());
    assert!(
        output.status.success(),
        "exit={:?} stderr={} stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let parsed = parse_stdout(&output);
    let data = assert_envelope(&parsed, CONFORMANCE_TEST_SCHEMA_VERSION);
    assert_eq!(data["summary"]["pass"], 1);
    assert_eq!(result(data, "metadata/isolated.harn")["outcome"], "pass");
}

#[test]
fn conformance_json_schema_catalog_entry_is_registered() {
    let output = Command::new(binary_path())
        .args(["--json-schemas", "--command", "test conformance"])
        .output()
        .expect("spawn harn --json-schemas --command test conformance");
    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = parse_stdout(&output);
    let data = assert_envelope(&parsed, harn_cli::json_envelope::CATALOG_SCHEMA_VERSION);
    let entries = data.as_array().expect("catalog data array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["command"], "test conformance");
    assert_eq!(
        entries[0]["schemaVersion"],
        serde_json::json!(CONFORMANCE_TEST_SCHEMA_VERSION)
    );
}

const PASSING_SOURCE: &str =
    "pipeline test(harness: Harness, task: unknown) {\n  harness.stdio.println(true)\n}\n";

/// The defect this guard exists for, at the boundary where it bit: a fixture
/// added without its `.expected` sibling selected zero tests and exited 0, so
/// an author probing for a RED read the green exit as evidence.
#[test]
fn conformance_exits_non_zero_when_the_only_fixture_has_no_expectation_sibling() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_source_without_expectation(temp.path(), "orphan.harn", PASSING_SOURCE);

    let output = run_conformance(temp.path(), &[]);

    assert!(
        !output.status.success(),
        "a run that executed nothing must not exit 0; stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no conformance tests ran"),
        "stderr must name the verdict: {stderr}"
    );
    assert!(
        stderr.contains("1 of 1 selected file(s) have no .expected"),
        "stderr must name the missing-sibling cause: {stderr}"
    );
}

/// The opt-out has to actually restore the old behavior, or the escape hatch
/// the flag advertises does not exist.
#[test]
fn allow_empty_restores_exit_zero_for_the_same_empty_selection() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_source_without_expectation(temp.path(), "orphan.harn", PASSING_SOURCE);

    let guarded = run_conformance(temp.path(), &[]);
    let opted_out = run_conformance(temp.path(), &["--allow-empty"]);

    assert!(!guarded.status.success(), "guarded run should be non-zero");
    assert!(
        opted_out.status.success(),
        "--allow-empty should accept the identical selection; stdout={}\nstderr={}",
        String::from_utf8_lossy(&opted_out.stdout),
        String::from_utf8_lossy(&opted_out.stderr),
    );
}

/// The negative control. A guard that reddens healthy runs is worse than the
/// defect it replaces, so a normal selection must be untouched.
#[test]
fn conformance_still_exits_zero_when_at_least_one_test_runs() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(temp.path(), "pass.harn", PASSING_SOURCE, "true\n");
    // A library module alongside it: having no sibling is normal and must not
    // fail a run that did execute something.
    write_source_without_expectation(temp.path(), "_common.harn", PASSING_SOURCE);

    let output = run_conformance(temp.path(), &[]);

    assert!(
        output.status.success(),
        "a run with one passing test must stay green; stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed, 0 failed"));
}

/// An over-narrow filter is the other way a run goes vacuous, and the one the
/// original report noted would have hidden the defect entirely.
#[test]
fn conformance_exits_non_zero_when_the_filter_matches_nothing() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(temp.path(), "pass.harn", PASSING_SOURCE, "true\n");

    let output = run_conformance(temp.path(), &["--filter", "no_such_conformance_test"]);

    assert!(
        !output.status.success(),
        "an empty filter result must be red"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no_such_conformance_test"),
        "stderr must name the filter that selected nothing: {stderr}"
    );
}

/// The parallel path has its own summary and its own exit, so it needs its own
/// proof — and its workers must not mistake an empty shard for this failure.
#[test]
fn parallel_conformance_exits_non_zero_on_an_empty_selection() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_source_without_expectation(temp.path(), "orphan.harn", PASSING_SOURCE);

    let guarded = run_conformance(temp.path(), &["--parallel", "--jobs", "2"]);
    let opted_out = run_conformance(temp.path(), &["--parallel", "--jobs", "2", "--allow-empty"]);

    assert!(
        !guarded.status.success(),
        "parallel run that executed nothing must not exit 0; stdout={}\nstderr={}",
        String::from_utf8_lossy(&guarded.stdout),
        String::from_utf8_lossy(&guarded.stderr),
    );
    assert!(
        String::from_utf8_lossy(&guarded.stderr).contains("no conformance tests ran"),
        "stderr={}",
        String::from_utf8_lossy(&guarded.stderr)
    );
    assert!(
        opted_out.status.success(),
        "--allow-empty should accept it on the parallel path too; stderr={}",
        String::from_utf8_lossy(&opted_out.stderr)
    );
}

#[test]
fn conformance_json_reports_the_empty_selection_as_a_closed_error_code() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_source_without_expectation(temp.path(), "orphan.harn", PASSING_SOURCE);

    let output = run_conformance_json(temp.path());

    assert!(!output.status.success());
    let parsed = parse_stdout(&output);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "conformance_empty_selection");
    assert_eq!(parsed["schemaVersion"], CONFORMANCE_TEST_SCHEMA_VERSION);
}

#[test]
fn parallel_conformance_json_reports_the_same_empty_selection_error_code() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_source_without_expectation(temp.path(), "orphan.harn", PASSING_SOURCE);

    let output = run_parallel_conformance_json(temp.path());

    assert!(!output.status.success());
    let parsed = parse_stdout(&output);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "conformance_empty_selection");
}
