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

    // Stable top-level keys consumed by Burin Code preflight automation.
    for key in [
        "schema_version",
        "harn_version",
        "checks",
        "hardware",
        "next_step",
        "summary",
    ] {
        assert!(parsed.get(key).is_some(), "missing top-level key '{key}'");
    }
    assert_eq!(
        parsed["schema_version"].as_str(),
        Some("1"),
        "schema_version must remain '1' until a documented breaking change"
    );

    let summary = &parsed["summary"];
    for key in ["ok", "warn", "fail", "skip", "blocked_flows"] {
        assert!(
            summary.get(key).is_some(),
            "summary missing key '{key}': {summary}"
        );
    }
    assert!(summary["blocked_flows"].is_array());

    let checks = parsed["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty(), "no checks emitted");

    let mut seen_ids = std::collections::HashSet::new();
    let required_ids = [
        "harn_version",
        "rustc",
        "cargo",
        "cargo-nextest",
        "sccache",
        "actionlint",
        "platform:file-watcher",
        "platform:browser-opener",
    ];
    for check in checks {
        for field in ["id", "status", "label", "detail"] {
            assert!(
                check.get(field).is_some(),
                "check missing required field '{field}': {check}"
            );
        }
        assert!(check.get("blocks").map(|v| v.is_array()).unwrap_or(false));
        // fix_command and docs_url are optional but the keys must be present.
        for optional in ["fix_command", "docs_url"] {
            assert!(
                check.get(optional).is_some(),
                "check missing optional key '{optional}': {check}"
            );
        }
        if let Some(id) = check.get("id").and_then(|v| v.as_str()) {
            seen_ids.insert(id.to_string());
        }
    }
    for id in required_ids {
        assert!(
            seen_ids.contains(id),
            "expected check '{id}' in JSON output. Saw: {seen_ids:?}"
        );
    }
}

/// Asserts that no check ever surfaces a literal env-var value in `detail`.
/// We ship credentials by name only so doctor output is safe to paste in
/// support threads, screenshots, or CI logs.
#[test]
fn doctor_never_prints_secret_values() {
    // Set a known sentinel for an env var doctor inspects, run doctor, then
    // make sure the value never appears in stdout.
    let sentinel = "SUPER_SECRET_SENTINEL_VALUE_12345";
    let output = Command::new(binary_path())
        .args(["doctor", "--json", "--no-network"])
        .env("ANTHROPIC_API_KEY", sentinel)
        .env("OPENAI_API_KEY", sentinel)
        .output()
        .expect("spawn harn doctor");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains(sentinel),
        "doctor leaked a secret value into output"
    );
}
