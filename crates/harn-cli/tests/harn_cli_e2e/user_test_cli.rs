//! End-to-end coverage for user `harn test` runner output.

use std::process::Command;

#[path = "user_test_cli/operator_grants.rs"]
mod operator_grants;
#[path = "user_test_cli/process_egress.rs"]
mod process_egress;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn affected_test_plan_reports_selection_and_safe_fallback_without_running_tests() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(suite.join("lib")).expect("create suite");
    std::fs::write(
        suite.join("lib/shared.harn"),
        "pub fn value() -> int { return 1 }\n",
    )
    .expect("write shared module");
    std::fs::write(
        suite.join("test_direct.harn"),
        "import { value } from \"lib/shared.harn\"\npipeline test_direct(_task) { assert_eq(value(), 999) }\n",
    )
    .expect("write direct test");
    std::fs::write(
        suite.join("test_unrelated.harn"),
        "pipeline test_unrelated(_task) { assert_eq(1, 999) }\n",
    )
    .expect("write unrelated test");
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    git(temp.path(), &["config", "user.name", "Harn Test"]);
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);
    let base = git(temp.path(), &["rev-parse", "HEAD"]);

    std::fs::write(
        suite.join("lib/shared.harn"),
        "pub fn value() -> int { return 2 }\n",
    )
    .expect("change shared module");
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-q", "-m", "change module"]);

    let selected = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["test", "suite", "--affected-from", &base, "--plan"])
        .output()
        .expect("spawn affected test plan");
    assert!(
        selected.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&selected.stdout),
        String::from_utf8_lossy(&selected.stderr)
    );
    let selected_json: serde_json::Value =
        serde_json::from_slice(&selected.stdout).expect("selected plan JSON");
    assert_eq!(selected_json["schema_version"], 1);
    assert_eq!(selected_json["kind"], "harn.test.affected_plan");
    assert_eq!(selected_json["mode"], "selected");
    assert_eq!(selected_json["test_file_count"], 1);
    assert_eq!(selected_json["test_files"][0], "suite/test_direct.harn");
    assert!(!String::from_utf8_lossy(&selected.stdout).contains("PASS"));

    let harn_base = git(temp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(temp.path().join("policy.toml"), "changed = true\n")
        .expect("write non-Harn input");
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-q", "-m", "change policy"]);

    let fallback = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["test", "suite", "--affected-from", &harn_base, "--plan"])
        .output()
        .expect("spawn fallback test plan");
    assert!(fallback.status.success());
    let fallback_json: serde_json::Value =
        serde_json::from_slice(&fallback.stdout).expect("fallback plan JSON");
    assert_eq!(fallback_json["mode"], "full");
    assert_eq!(fallback_json["test_file_count"], 2);
    assert!(fallback_json["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("changed non-Harn input policy.toml")));

    let invalid = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["test", "suite", "--plan"])
        .output()
        .expect("spawn invalid test plan");
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr)
        .contains("--plan requires --affected-from <git-ref>"));
}

#[test]
fn empty_affected_selection_writes_zero_case_machine_reports() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    std::fs::write(
        suite.join("test_must_not_run.harn"),
        "pipeline test_must_not_run(_task) { assert_eq(1, 2) }\n",
    )
    .expect("write failing sentinel test");
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    git(temp.path(), &["config", "user.name", "Harn Test"]);
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);

    let junit = temp.path().join("report.xml");
    let json_out = temp.path().join("report.json");
    let output = Command::new(binary_path())
        .current_dir(temp.path())
        .args([
            "test",
            "suite",
            "--affected-from",
            "HEAD",
            "--shard-index",
            "1",
            "--shard-total",
            "1",
            "--timeout",
            "120000",
            "--parallel",
            "--timing",
            "--junit",
            junit.to_str().expect("JUnit path is UTF-8"),
            "--json-out",
            json_out.to_str().expect("JSON report path is UTF-8"),
        ])
        .output()
        .expect("spawn empty affected test selection");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_out).expect("read JSON report"))
            .expect("parse JSON report");
    assert_eq!(
        report["schemaVersion"],
        harn_cli::test_report::USER_TEST_REPORT_SCHEMA_VERSION
    );
    assert_eq!(report["summary"]["total"], 0);
    assert_eq!(report["summary"]["passed"], 0);
    assert_eq!(report["summary"]["failed"], 0);
    assert_eq!(report["cases"], serde_json::json!([]));

    let junit_xml = std::fs::read_to_string(&junit).expect("read JUnit report");
    assert!(junit_xml.contains(r#"tests="0""#), "{junit_xml}");
    assert!(!junit_xml.contains("<testcase"), "{junit_xml}");
}

#[test]
fn affected_test_reports_keep_suite_relative_case_identity() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(suite.join("lib")).expect("create suite");
    std::fs::write(
        suite.join("lib/shared.harn"),
        "pub fn value() -> int { return 1 }\n",
    )
    .expect("write shared module");
    std::fs::write(
        suite.join("test_direct.harn"),
        "import { value } from \"lib/shared.harn\"\npipeline test_direct(_task) { assert_eq(value() > 0, true) }\n",
    )
    .expect("write direct test");
    std::fs::write(
        suite.join("test_unrelated.harn"),
        "pipeline test_unrelated(_task) { assert(true) }\n",
    )
    .expect("write unrelated test");
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    git(temp.path(), &["config", "user.name", "Harn Test"]);
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-q", "-m", "base"]);
    let base = git(temp.path(), &["rev-parse", "HEAD"]);

    std::fs::write(
        suite.join("lib/shared.harn"),
        "pub fn value() -> int { return 2 }\n",
    )
    .expect("change shared module");
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-q", "-m", "change module"]);
    let json_out = temp.path().join("report.json");
    let junit = temp.path().join("report.xml");
    let output = Command::new(binary_path())
        .current_dir(temp.path())
        .args([
            "test",
            "suite",
            "--affected-from",
            &base,
            "--junit",
            junit.to_str().expect("JUnit report path is UTF-8"),
            "--json-out",
            json_out.to_str().expect("JSON report path is UTF-8"),
        ])
        .output()
        .expect("spawn affected test run");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_out).expect("read JSON report"))
            .expect("parse JSON report");
    let cases = report["cases"]
        .as_array()
        .expect("report cases are an array");
    assert_eq!(cases.len(), 1, "{report:#}");
    assert_eq!(
        report["root"],
        suite
            .canonicalize()
            .expect("canonical suite")
            .display()
            .to_string()
    );
    assert_eq!(cases[0]["file"], "test_direct.harn");
    assert_eq!(cases[0]["classname"], "test_direct.harn");
    let junit_xml = std::fs::read_to_string(&junit).expect("read JUnit report");
    assert!(
        junit_xml.contains(r#"classname="test_direct.harn" file="test_direct.harn""#),
        "{junit_xml}"
    );
}

#[test]
fn multiple_test_targets_share_one_relative_report_namespace() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite_a = temp.path().join("suite_a");
    let suite_b = temp.path().join("suite_b/nested");
    std::fs::create_dir_all(&suite_a).expect("create suite A");
    std::fs::create_dir_all(&suite_b).expect("create suite B");
    std::fs::write(
        suite_a.join("test_a.harn"),
        "pipeline test_a(_task) { assert(true) }\n",
    )
    .expect("write suite A test");
    std::fs::write(
        suite_b.join("test_b.harn"),
        "pipeline test_b(_task) { assert(true) }\n",
    )
    .expect("write suite B test");
    let json_out = temp.path().join("report.json");

    let output = Command::new(binary_path())
        .current_dir(temp.path())
        .args([
            "test",
            "suite_a",
            "--test-path",
            "suite_b/nested",
            "--json-out",
            json_out.to_str().expect("JSON report path is UTF-8"),
        ])
        .output()
        .expect("spawn multi-target test run");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_out).expect("read JSON report"))
            .expect("parse JSON report");
    assert_eq!(
        report["root"],
        temp.path()
            .canonicalize()
            .expect("canonical tempdir")
            .display()
            .to_string()
    );
    let mut files = report["cases"]
        .as_array()
        .expect("report cases are an array")
        .iter()
        .map(|case| case["file"].as_str().expect("case file").to_string())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files, ["suite_a/test_a.harn", "suite_b/nested/test_b.harn"]);
}

#[test]
fn std_testing_temp_dir_works_in_the_default_run_sandbox() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
import { with_temp_dir } from "std/testing"

pipeline main(harness: Harness) {
  const result = with_temp_dir(
    harness.fs,
    { dir ->
      harness.fs.write_text(dir + "/value.txt", "sandboxed")
      {dir: dir, value: harness.fs.read_text(dir + "/value.txt")}
    },
  )
  assert_eq(result.value, "sandboxed")
  assert_eq(harness.fs.exists(result.dir), false)
  harness.stdio.println("sandbox-temp-ok")
}
"#,
    )
    .expect("write sandbox fixture");

    let output = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["run", script.to_str().expect("script path is UTF-8")])
        .output()
        .expect("spawn sandboxed harn run");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "sandbox-temp-ok\n");
}

#[test]
fn user_tests_emit_progress_and_timing_summary() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    std::fs::write(
        suite.join("test_alpha.harn"),
        r"
pipeline test_alpha(task) {
  assert_eq(1, 1)
}
",
    )
    .expect("write alpha");
    std::fs::write(
        suite.join("test_beta.harn"),
        r"
pipeline test_beta(task) {
  assert_eq(2, 2)
}
",
    )
    .expect("write beta");

    let output = Command::new(binary_path())
        .args([
            "test",
            suite.to_str().unwrap(),
            "--verbose",
            "--timing",
            "--timeout",
            "10000",
        ])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Running 2 tests from 2 files with 1 worker (sequential scheduling)"),
        "stdout did not include the suite banner:\n{stdout}"
    );
    assert!(stdout.contains("RUN   test_alpha"));
    assert!(stdout.contains("PASS"));
    assert!(stdout.contains("Latency: p50="));
    assert!(stdout.contains("p90="));
    assert!(stdout.contains("Per-test detail: avg="));
    assert!(stdout.contains("Slowest 2 tests:"));
    assert!(stdout.contains("Slowest 2 files:"));
    assert!(stdout.contains("Module attribution (overlaps phases):"));

    let first_run = stdout.find("RUN   test_alpha").expect("alpha start");
    let first_pass = stdout.find("PASS").expect("pass output");
    assert!(
        first_run < first_pass,
        "test-start progress should be emitted before PASS:\n{stdout}"
    );
}

#[test]
fn diagnose_environment_reports_cold_import_graph_before_user_test_execution() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    for index in (0..32).rev() {
        let helpers = (0..32)
            .map(|helper| format!("fn helper_{index}_{helper}() {{ return {helper} }}\n"))
            .collect::<String>();
        let source = if index == 31 {
            format!("{helpers}\npub fn value_31() {{ return 511 }}\n")
        } else {
            format!(
                "{helpers}\nimport {{ value_{} }} from \"./module_{}\"\npub fn value_{index}() {{ return value_{}() }}\n",
                index + 1,
                index + 1,
                index + 1,
            )
        };
        std::fs::write(suite.join(format!("module_{index}.harn")), source)
            .expect("write cold graph module");
    }
    std::fs::write(
        suite.join("test_cold_graph.harn"),
        r#"
import { value_0 } from "./module_0"
pipeline test_cold_graph(_task) { assert_eq(value_0(), 511) }
"#,
    )
    .expect("write test");

    let quiet_output = Command::new(binary_path())
        .env("HARN_CACHE_DIR", temp.path().join("quiet-bytecode-cache"))
        .args([
            "test",
            suite.to_str().expect("suite path is UTF-8"),
            "--timeout",
            "5000",
            "--timing",
        ])
        .output()
        .expect("spawn harn test without diagnostics");
    assert!(quiet_output.status.success());
    assert!(
        !String::from_utf8_lossy(&quiet_output.stderr).contains("[harn test diag]"),
        "diagnostics must remain opt-in"
    );

    let output = Command::new(binary_path())
        .env("HARN_CACHE_DIR", temp.path().join("bytecode-cache"))
        .env("HARN_TEST_DIAGNOSE", "1")
        .args([
            "test",
            suite.to_str().expect("suite path is UTF-8"),
            "--timeout",
            "5000",
            "--timing",
        ])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let phase_line = stdout
        .lines()
        .find(|line| line.starts_with("Phase totals:"))
        .unwrap_or_else(|| panic!("missing phase totals:\n{stdout}"));
    let compile_ms = phase_line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("compile="))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("missing compile phase value: {phase_line}"));

    assert!(
        compile_ms > 0,
        "a deliberately cold 512-function module must be visible in the suite compile phase: {phase_line}"
    );
    assert!(
        stdout.contains("compile=") && stdout.contains("(32 modules)"),
        "aggregate module attribution must own the cold compile:\n{stdout}"
    );
    assert!(
        stderr.contains("modules_compiled=0"),
        "the case diagnostic must prove compilation finished before its execution clock:\n{stderr}"
    );
}

#[test]
fn empty_user_suite_pins_default_latency_representation() {
    let temp = tempfile::TempDir::new().expect("tempdir");

    let output = Command::new(binary_path())
        .args(["test", temp.path().to_str().unwrap()])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No test pipelines found"), "{stdout}");
    assert!(
        stdout.contains("Latency: p50=n/a  p90=n/a (0 samples)"),
        "{stdout}"
    );
}

#[test]
fn default_user_test_output_includes_latency_summary() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let test_file = temp.path().join("test_latency.harn");
    std::fs::write(
        &test_file,
        "pipeline test_latency(task) { assert_eq(1, 1) }\n",
    )
    .expect("write test");

    let output = Command::new(binary_path())
        .args(["test", test_file.to_str().unwrap(), "--timeout", "10000"])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let latency_lines = stdout
        .lines()
        .filter(|line| line.starts_with("Latency: "))
        .collect::<Vec<_>>();
    assert_eq!(latency_lines.len(), 1, "{stdout}");
    let latency = latency_lines[0]
        .strip_prefix("Latency: p50=")
        .expect("p50 prefix");
    let (p50, p90) = latency.split_once(" ms  p90=").expect("p90 separator");
    assert!(p50.parse::<u64>().is_ok(), "{latency}");
    assert!(
        p90.strip_suffix(" ms")
            .is_some_and(|value| value.parse::<u64>().is_ok()),
        "{latency}"
    );
    assert!(!stdout.contains("Per-test detail:"), "{stdout}");
}

#[test]
fn user_tests_register_project_host_capability_manifest_for_mocks() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    std::fs::write(
        temp.path().join("harn.toml"),
        "[check]\nhost_capabilities_path = \"host-capabilities.json\"\n",
    )
    .expect("write manifest");
    std::fs::write(
        temp.path().join("host-capabilities.json"),
        r#"{"synthetic_fixture":["answer"]}"#,
    )
    .expect("write host capabilities");
    std::fs::write(
        suite.join("test_manifest_mock.harn"),
        r#"
import { with_capability_fixtures } from "std/testing"

pipeline test_manifest_mock(harness: Harness, task) {
  with_capability_fixtures(
    harness.testing,
    [{capability: "synthetic_fixture", method: "answer", result: 42}],
    { _ ->
      assert_eq(len(harness.testing.calls()), 0)
    },
  )
}
"#,
    )
    .expect("write test");

    let output = Command::new(binary_path())
        .args(["test", suite.to_str().unwrap(), "--timeout", "10000"])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scoped_capability_fixtures_intercept_host_calls_and_preserve_thrown_values() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    std::fs::write(
        suite.join("test_scoped_fixture.harn"),
        r#"
import { with_capability_fixtures } from "std/testing"

pipeline test_scoped_fixture(harness: Harness, task) {
  const answer = with_capability_fixtures(
    harness.testing,
    [{capability: "workspace", method: "project_root", result: "/tmp/project"}],
    { _ -> harness.workspace.project_root({}) },
  )
  assert_eq(answer, "/tmp/project")

  const result = with_capability_fixtures(
    harness.testing,
    [{capability: "runtime", method: "set_result", result: "captured", unregistered_ok: true}],
    { _ -> harness.runtime.set_result({status: "done"}) },
  )
  assert_eq(result, "captured")

  const caught = try {
    with_capability_fixtures(
      harness.testing,
      [{capability: "workspace", method: "project_root", result: "/tmp/project"}],
      { _ -> throw "fixture-body-failure" },
    )
    "unreachable"
  } catch (error) {
    error
  }
  assert_eq(caught, "fixture-body-failure")
  assert_eq(len(harness.testing.calls()), 0)
}
"#,
    )
    .expect("write test");

    let output = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["test", suite.to_str().unwrap(), "--timeout", "10000"])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn user_tests_reject_mock_operation_missing_from_project_manifest() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    std::fs::write(
        temp.path().join("harn.toml"),
        "[check]\nhost_capabilities.synthetic_fixture = [\"answer\"]\n",
    )
    .expect("write manifest");
    std::fs::write(
        suite.join("test_manifest_typo.harn"),
        r#"
import { with_capability_fixtures } from "std/testing"

pipeline test_manifest_typo(harness: Harness, task) {
  with_capability_fixtures(
    harness.testing,
    [{capability: "synthetic_fixture", method: "asnwer", result: 42}],
    { _ -> nil },
  )
}
"#,
    )
    .expect("write test");

    let output = Command::new(binary_path())
        .args(["test", suite.to_str().unwrap(), "--timeout", "10000"])
        .output()
        .expect("spawn harn test");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "unexpected success:\n{stdout}");
    assert!(
        stdout.contains("unknown capability or host operation `synthetic_fixture.asnwer`"),
        "missing strict registration failure:\n{stdout}"
    );
}
