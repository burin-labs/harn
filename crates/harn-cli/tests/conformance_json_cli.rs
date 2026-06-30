//! End-to-end coverage for `harn test conformance --json`.

use std::process::{Command, Output};

use harn_cli::tests::common::json_envelope::assert_envelope;
use serde_json::Value;

const CONFORMANCE_TEST_SCHEMA_VERSION: u32 = 1;

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

fn run_conformance_json(root: &std::path::Path) -> Output {
    Command::new(binary_path())
        .args(["test", "conformance", "--json", "--timeout", "10000"])
        .current_dir(root)
        .env("HARN_EVENT_LOG_BACKEND", "memory")
        .output()
        .expect("spawn harn test conformance --json")
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
        "pipeline test(task) {\n  log(\"pass\")\n}\n",
        "[harn] pass\n",
    );
    write_fixture(
        temp.path(),
        "xfail_expected.harn",
        "// @xfail: tracked in #999\npipeline test(task) {\n  log(\"actual\")\n}\n",
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
fn conformance_json_fails_on_failures_and_unexpected_xfail_passes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    write_fixture(
        temp.path(),
        "fail.harn",
        "pipeline test(task) {\n  log(\"actual\")\n}\n",
        "[harn] expected\n",
    );
    write_fixture(
        temp.path(),
        "xfail_unexpected_pass.harn",
        "// @xfail: tracked in #999\npipeline test(task) {\n  log(\"fixed\")\n}\n",
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
        "pipeline test(task) {\n  assert_eq(metadata_get(\".\", \"classification\"), nil)\n  let stale = metadata_stale(\".\")\n  assert_eq(stale.any_stale, false)\n  log(\"isolated\")\n}\n",
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
