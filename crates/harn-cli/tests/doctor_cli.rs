//! End-to-end smoke for `harn doctor --json`.
//!
//! Calls the binary as a subprocess so we exercise the actual CLI
//! parser + JSON serialization path. Kept minimal so it runs in seconds.

use std::process::Command;

fn binary_path() -> std::path::PathBuf {
    // CARGO_BIN_EXE_<name> is set by Cargo for integration tests in the
    // crate that owns the binary.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

#[test]
fn doctor_json_smoke() {
    let output = Command::new(binary_path())
        .args(["doctor", "--json", "--no-network"])
        .output()
        .expect("spawn harn doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim_start().starts_with('{'),
        "stdout is not JSON: {stdout}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("doctor --json is valid JSON");
    assert!(parsed.get("checks").is_some(), "missing 'checks' key");
    assert!(parsed.get("hardware").is_some(), "missing 'hardware' key");
    assert!(parsed.get("next_step").is_some(), "missing 'next_step' key");
    assert!(
        parsed.get("harn_version").is_some(),
        "missing 'harn_version' key"
    );
    let checks = parsed["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty(), "no checks emitted");
    for check in checks {
        assert!(check.get("id").and_then(|v| v.as_str()).is_some());
        assert!(check.get("status").and_then(|v| v.as_str()).is_some());
    }
}
