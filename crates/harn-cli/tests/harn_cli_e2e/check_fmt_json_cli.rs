//! End-to-end coverage for `harn check --json` and `harn fmt --json`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

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
    std::fs::write(&script, "pipeline main(task: unknown) {\n  return 1\n}\n")
        .expect("write script");

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

    std::fs::write(&script, "pipeline main(task: unknown){return 1}\n").expect("rewrite script");
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
    std::fs::write(&script, "pipeline main(task: unknown) {\n  return 1\n}\n")
        .expect("write script");

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

    std::fs::write(&script, "const p = Point { x: 3, y: 4 }\n").expect("rewrite script");
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
fn check_reconciles_declared_and_served_host_capability_surfaces() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(&script, "pipeline main() {}\n").expect("write script");
    std::fs::write(
        temp.path().join("declared.json"),
        r#"{"synthetic":["served","phantom","runtime_only"]}"#,
    )
    .expect("write declarations");
    let served = temp.path().join("served.json");
    std::fs::write(&served, r#"{"synthetic":["served"]}"#).expect("write served surface");
    std::fs::write(
        temp.path().join("harn.toml"),
        r#"
[check]
host_capabilities_path = "declared.json"
host_served_capabilities_path = "served.json"
runtime_installed_host_operations = ["synthetic.runtime_only"]
"#,
    )
    .expect("write manifest");

    let failed = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["check", "--json", "main.harn"])
        .output()
        .expect("spawn harn check --json");
    assert!(
        !failed.status.success(),
        "phantom operation must fail check"
    );
    let parsed = stdout_json(&failed);
    let diagnostics = parsed["data"]["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic["code"] == "HARN-CAP-008")
            .count(),
        1,
        "only the non-exempt phantom should fail: {parsed}"
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains("synthetic.phantom"))
    }));

    std::fs::write(&served, r#"{"synthetic":["served","phantom"]}"#)
        .expect("serve formerly phantom operation");
    let passed = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["check", "--json", "main.harn"])
        .output()
        .expect("spawn reconciled harn check --json");
    assert!(
        passed.status.success(),
        "reconciled surface should pass: {}",
        String::from_utf8_lossy(&passed.stderr)
    );

    std::fs::write(temp.path().join("declared.json"), "not valid JSON or TOML")
        .expect("write malformed declarations");
    let malformed = Command::new(binary_path())
        .current_dir(temp.path())
        .args(["check", "--json", "main.harn"])
        .output()
        .expect("spawn harn check --json with malformed declarations");
    assert!(
        !malformed.status.success(),
        "malformed declarations must not pass reconciliation"
    );
    let parsed = stdout_json(&malformed);
    let diagnostics = parsed["data"]["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["code"] == "HARN-CAP-008"
            && diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("failed to parse declared host operations"))
    }));
}

#[test]
fn check_reports_bytecode_compile_errors_not_just_type_errors() {
    // `harn check` is a "will this run?" gate: it must catch errors the type
    // checker does not model but that stop `harn run`. Interpolation holes are
    // parsed from string literals during bytecode compilation, so malformed
    // interpolation must surface here as a HARN-CMP-001 diagnostic and a failed
    // check.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        "pipeline main(task: unknown) {\n  return \"value ${1 2}\"\n}\n",
    )
    .expect("write script");

    let failed = Command::new(binary_path())
        .args(["check", "--json", script.to_str().unwrap()])
        .output()
        .expect("spawn harn check --json");
    assert!(
        !failed.status.success(),
        "a bytecode compile error should fail check; stderr:\n{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    let parsed = stdout_json(&failed);
    assert_eq!(parsed["ok"], false);
    assert!(
        parsed["data"]["files"][0]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|diag| diag["code"].as_str() == Some("HARN-CMP-001")),
        "expected a HARN-CMP-001 compile diagnostic: {parsed}"
    );
}

#[test]
fn check_json_exits_successfully_when_stdout_consumer_closes_early() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    for index in 0..300 {
        let script = temp.path().join(format!("case_{index:03}.harn"));
        std::fs::write(
            &script,
            format!("pipeline main(task: unknown) {{\n  return {index}\n}}\n"),
        )
        .expect("write script");
    }

    let mut child = Command::new(binary_path())
        .args(["check", "--json", temp.path().to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn harn check --json");

    let stdout = child.stdout.take().expect("stdout pipe");
    let mut reader = BufReader::new(stdout);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).expect("read first line");
    drop(reader);

    let output = child.wait_with_output().expect("wait for harn check");
    assert!(
        output.status.success(),
        "consumer-close should be a clean exit\nstatus: {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("failed printing to stdout"),
        "broken pipe panic leaked to stderr:\n{stderr}"
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
    for command in ["check", "fmt", "check --provider-matrix"] {
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
