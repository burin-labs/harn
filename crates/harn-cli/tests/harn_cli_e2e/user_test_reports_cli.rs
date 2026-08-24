//! End-to-end coverage for `harn test --junit` / `--json-out` on user
//! Harn tests. Regression guard for issue #2146: the CLI used to accept
//! both flags but silently drop them for user-test runs.

use crate::test_util;

use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;
use test_util::process::harn_e2e_command;

const PASS_PIPELINE: &str =
    "pipeline test_pass(harness: Harness, task) {\n  harness.stdio.log(\"ok\")\n}\n";
const FAIL_PIPELINE: &str = "pipeline test_fail(task) {\n  assert_eq(1, 2)\n}\n";

fn write_fixture(root: &std::path::Path, relative: &str, source: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    std::fs::write(&path, source).expect("write fixture");
}

fn run_test(temp: &TempDir, extra: &[&str]) -> Output {
    let mut args = vec!["test", "suite", "--timeout", "10000"];
    args.extend_from_slice(extra);
    harn_e2e_command()
        .current_dir(temp.path())
        .args(&args)
        .output()
        .expect("spawn harn test")
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn junit_and_json_out_written_for_passing_user_tests() {
    let temp = TempDir::new().expect("tempdir");
    write_fixture(temp.path(), "suite/test_pass.harn", PASS_PIPELINE);

    let junit = temp.path().join("report.xml");
    let json_out = temp.path().join("report.json");

    let output = run_test(
        &temp,
        &[
            "--junit",
            junit.to_str().unwrap(),
            "--json-out",
            json_out.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(junit.is_file(), "JUnit report was not written");
    assert!(json_out.is_file(), "JSON report was not written");

    let xml = std::fs::read_to_string(&junit).expect("read JUnit");
    assert!(
        xml.contains("<testsuites"),
        "missing testsuites wrapper: {xml}"
    );
    assert!(
        xml.contains(r#"name="test_pass""#),
        "missing testcase name: {xml}"
    );
    assert!(
        xml.contains(r#"failures="0""#),
        "expected zero failures: {xml}"
    );

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&json_out).expect("read JSON"))
        .expect("parse JSON");
    assert_eq!(
        json["schemaVersion"],
        harn_cli::test_report::USER_TEST_REPORT_SCHEMA_VERSION
    );
    assert_eq!(json["summary"]["total"], 1);
    assert_eq!(json["summary"]["passed"], 1);
    assert_eq!(json["summary"]["failed"], 0);
    assert_eq!(json["cases"][0]["name"], "test_pass");
    assert_eq!(json["cases"][0]["outcome"], "passed");
    assert_eq!(json["timing"]["sample_count"], 1);
    assert!(json["timing"]["p90_ms"].as_u64().is_some());
    assert!(json["aggregate"]["setup_ms"].as_u64().is_some());
    assert!(json["cases"][0]["phases"]["modules"]["modules_loaded"]
        .as_u64()
        .is_some());
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn timing_receipt_drives_dominant_shard_through_the_cli() {
    let temp = TempDir::new().expect("tempdir");
    write_fixture(
        temp.path(),
        "suite/test_costs.harn",
        r#"
import { timed } from "std/timing"

pipeline test_big(harness: Harness, task) {
  const measured = timed(harness.clock, "sweep.big", {case: "big"}, { -> task })
  assert_eq(measured.result, task)
}

pipeline test_small_a(task) { return task }
pipeline test_small_b(task) { return task }
"#,
    );
    let baseline_path = temp.path().join("baseline.json");
    let baseline_output = run_test(
        &temp,
        &[
            "--json-out",
            baseline_path.to_str().unwrap(),
            "--timing-environment",
            "github-linux-x64",
        ],
    );
    assert!(
        baseline_output.status.success(),
        "baseline run failed: {}",
        String::from_utf8_lossy(&baseline_output.stderr)
    );
    let mut baseline: Value =
        serde_json::from_slice(&std::fs::read(&baseline_path).unwrap()).unwrap();
    assert_eq!(baseline["timingEnvironment"], "github-linux-x64");
    let big = baseline["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "test_big")
        .expect("baseline contains test_big");
    assert_eq!(big["timing_spans"][0]["name"], "sweep.big");
    for case in baseline["cases"].as_array_mut().unwrap() {
        case["phases"]["execute_ms"] = match case["name"].as_str().unwrap() {
            "test_big" => serde_json::json!(500),
            "test_small_a" => serde_json::json!(40),
            "test_small_b" => serde_json::json!(30),
            name => panic!("unexpected case {name}"),
        };
    }
    std::fs::write(
        &baseline_path,
        serde_json::to_vec_pretty(&baseline).unwrap(),
    )
    .unwrap();

    let shard_path = temp.path().join("shard.json");
    let shard_output = run_test(
        &temp,
        &[
            "--shard-index",
            "1",
            "--shard-total",
            "2",
            "--timing-baseline",
            baseline_path.to_str().unwrap(),
            "--timing-environment",
            "github-linux-x64",
            "--json-out",
            shard_path.to_str().unwrap(),
        ],
    );
    assert!(
        shard_output.status.success(),
        "shard run failed: {}",
        String::from_utf8_lossy(&shard_output.stderr)
    );
    let shard: Value = serde_json::from_slice(&std::fs::read(&shard_path).unwrap()).unwrap();
    assert_eq!(shard["cases"].as_array().unwrap().len(), 1);
    assert_eq!(shard["cases"][0]["name"], "test_big");
    assert_eq!(shard["shard_plan"]["dominant_case"]["name"], "test_big");
    assert_eq!(
        shard["shard_plan"]["dominant_case"]["file"],
        "test_costs.harn"
    );
    assert!(shard.get("cost_regressions").is_none());
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn junit_and_json_out_capture_failures_with_exit_code_one() {
    let temp = TempDir::new().expect("tempdir");
    write_fixture(temp.path(), "suite/test_pass.harn", PASS_PIPELINE);
    write_fixture(temp.path(), "suite/test_fail.harn", FAIL_PIPELINE);

    let junit = temp.path().join("report.xml");
    let json_out = temp.path().join("report.json");

    let output = run_test(
        &temp,
        &[
            "--junit",
            junit.to_str().unwrap(),
            "--json-out",
            json_out.to_str().unwrap(),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 on failure, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let xml = std::fs::read_to_string(&junit).expect("read JUnit");
    assert!(
        xml.contains(r#"failures="1""#),
        "expected 1 failure in XML: {xml}"
    );
    assert!(
        xml.contains("<failure"),
        "expected <failure> element in XML: {xml}"
    );

    let json: Value = serde_json::from_str(&std::fs::read_to_string(&json_out).expect("read JSON"))
        .expect("parse JSON");
    assert_eq!(json["summary"]["total"], 2);
    assert_eq!(json["summary"]["passed"], 1);
    assert_eq!(json["summary"]["failed"], 1);
    let cases = json["cases"].as_array().expect("cases array");
    let fail_case = cases
        .iter()
        .find(|c| c["name"] == "test_fail")
        .expect("missing failing case");
    assert_eq!(fail_case["outcome"], "failed");
    assert!(
        fail_case["message"].as_str().is_some_and(|m| !m.is_empty()),
        "failure message missing: {fail_case}"
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn unwritable_report_path_fails_loudly_before_running_tests() {
    let temp = TempDir::new().expect("tempdir");
    write_fixture(temp.path(), "suite/test_pass.harn", PASS_PIPELINE);

    let bad = temp.path().join("does/not/exist/report.xml");
    let output = run_test(&temp, &["--junit", bad.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 when report directory is missing, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("report directory does not exist"),
        "expected diagnostic about missing directory, got: {stderr}"
    );
    assert!(
        !bad.exists(),
        "no partial JUnit file should have been written"
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn watch_mode_rejects_report_flags() {
    let temp = TempDir::new().expect("tempdir");
    write_fixture(temp.path(), "suite/test_pass.harn", PASS_PIPELINE);

    let junit = temp.path().join("report.xml");
    let output = harn_e2e_command()
        .current_dir(temp.path())
        .args([
            "test",
            "suite",
            "--watch",
            "--junit",
            junit.to_str().unwrap(),
        ])
        .output()
        .expect("spawn harn test --watch --junit");
    assert!(
        !output.status.success(),
        "expected --watch + --junit combination to be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--watch"),
        "expected diagnostic mentioning --watch, got: {stderr}"
    );
}
