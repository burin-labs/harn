//! End-to-end smoke for `harn --json-schemas`.
//!
//! Spawns the binary so we exercise the real clap parser + envelope
//! serialization path.

use std::process::Command;

use harn_cli::json_envelope::CATALOG_SCHEMA_VERSION;
use harn_cli::tests::common::json_envelope::assert_envelope;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn parse_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim_start().starts_with('{'),
        "stdout is not JSON: {stdout}"
    );
    serde_json::from_str(stdout.trim()).expect("--json-schemas is valid JSON")
}

#[test]
fn json_schemas_lists_all_registered_commands() {
    let output = Command::new(binary_path())
        .args(["--json-schemas"])
        .output()
        .expect("spawn harn --json-schemas");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_stdout(&output);
    let data = assert_envelope(&parsed, CATALOG_SCHEMA_VERSION);
    let entries = data.as_array().expect("data is an array");
    assert!(!entries.is_empty(), "catalog should not be empty");
    assert!(
        entries.iter().any(|e| e["command"] == "doctor"),
        "doctor should be in the catalog: {entries:?}"
    );
    assert!(
        entries.iter().any(|e| e["command"] == "time run"),
        "`time run` should be in the catalog: {entries:?}"
    );
    for entry in entries {
        assert!(
            entry.get("command").and_then(|v| v.as_str()).is_some(),
            "entry missing command: {entry}"
        );
        let sv = entry
            .get("schemaVersion")
            .and_then(|v| v.as_u64())
            .expect("entry has schemaVersion");
        assert!(sv >= 1, "schemaVersion must be >= 1: {entry}");
    }
}

#[test]
fn json_schemas_filters_to_single_command() {
    let output = Command::new(binary_path())
        .args(["--json-schemas", "--command", "doctor"])
        .output()
        .expect("spawn harn --json-schemas --command doctor");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_stdout(&output);
    let data = assert_envelope(&parsed, CATALOG_SCHEMA_VERSION);
    let entries = data.as_array().expect("data is an array");
    assert_eq!(entries.len(), 1, "expected single-entry catalog");
    assert_eq!(entries[0]["command"], "doctor");
}

#[test]
fn json_schemas_unknown_command_returns_error_envelope_and_nonzero_exit() {
    let output = Command::new(binary_path())
        .args(["--json-schemas", "--command", "definitely-not-real"])
        .output()
        .expect("spawn harn --json-schemas --command <unknown>");
    assert!(
        !output.status.success(),
        "unknown command must exit nonzero"
    );

    let parsed = parse_stdout(&output);
    assert_eq!(parsed["schemaVersion"], CATALOG_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "schema_not_found");
}
