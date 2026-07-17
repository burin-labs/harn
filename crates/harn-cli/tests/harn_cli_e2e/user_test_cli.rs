//! End-to-end coverage for user `harn test` runner output.

use std::process::Command;

#[path = "user_test_cli/operator_grants.rs"]
mod operator_grants;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

#[test]
fn std_testing_temp_dir_works_in_the_default_run_sandbox() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
import { with_temp_dir } from "std/testing"

pipeline main() {
  const result = with_temp_dir(
    { dir ->
      harness.fs.write_text(dir + "/value.txt", "sandboxed")
      {dir: dir, value: harness.fs.read_text(dir + "/value.txt")}
    },
  )
  assert_eq(result.value, "sandboxed")
  assert_eq(harness.fs.exists(result.dir), false)
  __io_println("sandbox-temp-ok")
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
import { with_host_mocks } from "std/testing"

pipeline test_manifest_mock(task) {
  assert(!host_has("synthetic_fixture", "answer"))
  with_host_mocks(
    [{capability: "synthetic_fixture", operation: "answer", result: 42}],
    { _ ->
      assert(host_has("synthetic_fixture", "answer"))
      assert_eq(host_call("synthetic_fixture.answer", {}), 42)
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
import { with_host_mocks } from "std/testing"

pipeline test_manifest_typo(task) {
  with_host_mocks(
    [{capability: "synthetic_fixture", operation: "asnwer", result: 42}],
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
        stdout.contains("unregistered host operation synthetic_fixture.asnwer"),
        "missing strict registration failure:\n{stdout}"
    );
}
