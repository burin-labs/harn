//! End-to-end coverage for `harn check --json` and `harn fmt --json`.

use std::process::Command;

use harn_cli::tests::common::json_envelope::assert_envelope;

const CHECK_SCHEMA_VERSION: u32 = 1;
const FMT_SCHEMA_VERSION: u32 = 1;
const CATALOG_SCHEMA_VERSION: u32 = harn_cli::json_envelope::CATALOG_SCHEMA_VERSION;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

#[test]
fn fmt_check_json_reports_clean_and_drift_files() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(&script, "pipeline main(task) {\n  return 1\n}\n").expect("write script");

    let clean = Command::new(binary_path())
        .args(["fmt", "--check", "--json", script.to_str().unwrap()])
        .output()
        .expect("spawn harn fmt --check --json");
    assert!(
        clean.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let parsed = stdout_json(&clean);
    let data = assert_envelope(&parsed, FMT_SCHEMA_VERSION);
    assert_eq!(data["summary"]["already_formatted"], 1);
    assert_eq!(data["files"][0]["status"], "already_formatted");

    std::fs::write(&script, "pipeline main(task){return 1}\n").expect("rewrite script");
    let drift = Command::new(binary_path())
        .args(["fmt", "--check", "--json", script.to_str().unwrap()])
        .output()
        .expect("spawn harn fmt --check --json");
    assert!(!drift.status.success(), "drift should fail fmt --check");
    let parsed = stdout_json(&drift);
    assert_eq!(parsed["schemaVersion"], FMT_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "fmt_failed");
    assert_eq!(parsed["data"]["summary"]["errors"], 1);
    assert_eq!(parsed["data"]["files"][0]["status"], "error");
    assert!(parsed["data"]["files"][0]["diagnostics"][0]["code"]
        .as_str()
        .unwrap()
        .starts_with("HARN-FMT-"));
}

#[test]
fn check_json_reports_success_and_diagnostics() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(&script, "pipeline main(task) {\n  return 1\n}\n").expect("write script");

    let ok = Command::new(binary_path())
        .args(["check", "--json", script.to_str().unwrap()])
        .output()
        .expect("spawn harn check --json");
    assert!(
        ok.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let parsed = stdout_json(&ok);
    let data = assert_envelope(&parsed, CHECK_SCHEMA_VERSION);
    assert_eq!(data["summary"]["ok"], 1);
    assert_eq!(data["files"][0]["status"], "ok");

    std::fs::write(&script, "let p = Point { x: 3, y: 4 }\n").expect("rewrite script");
    let failed = Command::new(binary_path())
        .args(["check", "--json", script.to_str().unwrap()])
        .output()
        .expect("spawn failing harn check --json");
    assert!(!failed.status.success(), "type error should fail check");
    let parsed = stdout_json(&failed);
    assert_eq!(parsed["schemaVersion"], CHECK_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "check_failed");
    assert_eq!(parsed["data"]["summary"]["errors"], 1);
    assert_eq!(parsed["data"]["files"][0]["status"], "error");
    assert!(
        parsed["data"]["files"][0]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|diag| diag["code"].as_str().unwrap_or("").starts_with("HARN-TYP-")),
        "expected a type diagnostic: {parsed}"
    );
}

#[test]
fn check_matrix_json_uses_envelope_and_legacy_format_warns() {
    let output = Command::new(binary_path())
        .args(["check", "--provider-matrix", "--json"])
        .output()
        .expect("spawn harn check --provider-matrix --json");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = stdout_json(&output);
    let data = assert_envelope(&parsed, CHECK_SCHEMA_VERSION);
    assert!(data.as_array().is_some(), "matrix data should be an array");

    let legacy = Command::new(binary_path())
        .args(["check", "--provider-matrix", "--format", "json"])
        .output()
        .expect("spawn harn check --provider-matrix --format json");
    assert!(
        legacy.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&legacy.stderr)
    );
    let parsed = stdout_json(&legacy);
    let _ = assert_envelope(&parsed, CHECK_SCHEMA_VERSION);
    assert_eq!(parsed["warnings"][0]["code"], "deprecated.flag");
}

#[test]
fn check_and_fmt_are_registered_in_json_schema_catalog() {
    for command in ["check", "fmt", "check provider-matrix"] {
        let output = Command::new(binary_path())
            .args(["--json-schemas", "--command", command])
            .output()
            .expect("spawn harn --json-schemas");
        assert!(
            output.status.success(),
            "stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let parsed = stdout_json(&output);
        let data = assert_envelope(&parsed, CATALOG_SCHEMA_VERSION);
        assert_eq!(data.as_array().expect("entries").len(), 1);
        assert_eq!(data[0]["command"], command);
    }
}
