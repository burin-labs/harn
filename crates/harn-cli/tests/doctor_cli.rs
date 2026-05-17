//! End-to-end smoke for `harn doctor --json`.
//!
//! Calls the binary as a subprocess so we exercise the actual CLI
//! parser + JSON serialization path. Kept minimal so it runs in seconds.

use std::process::Command;

use harn_cli::json_envelope::CATALOG_SCHEMA_VERSION;
use harn_cli::tests::common::json_envelope::assert_envelope;

const DOCTOR_SCHEMA_VERSION: u32 = 2;

fn binary_path() -> std::path::PathBuf {
    // CARGO_BIN_EXE_<name> is set by Cargo for integration tests in the
    // crate that owns the binary.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

#[test]
fn doctor_json_smoke() {
    let output = Command::new(binary_path())
        .args(["doctor", "--json"])
        .output()
        .expect("spawn harn doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim_start().starts_with('{'),
        "stdout is not JSON: {stdout}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("doctor --json is valid JSON");
    let data = assert_envelope(&parsed, DOCTOR_SCHEMA_VERSION);

    // Stable top-level keys consumed by Burin Code preflight automation
    // (`checks`, `hardware`, `summary`, `next_step`) plus the new
    // capability-matrix fields introduced by #1785
    // (`host`, `targets`, `providers`, `capabilities`).
    for key in [
        "host",
        "targets",
        "providers",
        "capabilities",
        "checks",
        "hardware",
        "next_step",
        "summary",
    ] {
        assert!(data.get(key).is_some(), "missing data key '{key}': {data}");
    }

    // Host fingerprint.
    let host = &data["host"];
    assert!(host["os"].is_string());
    assert!(host["arch"].is_string());
    assert!(host["harn_version"].is_string());

    // Summary uses the new "blocking"/"warning" names per the spec.
    let summary = &data["summary"];
    for key in ["ok", "warning", "blocking", "skip", "blocked_flows"] {
        assert!(
            summary.get(key).is_some(),
            "summary missing key '{key}': {summary}"
        );
    }
    assert!(summary["blocked_flows"].is_array());

    // Targets matrix: each entry exposes triple + installed + buildable + reasons.
    let targets = data["targets"].as_array().expect("targets array");
    assert!(
        !targets.is_empty(),
        "doctor should list at least the canonical targets"
    );
    for target in targets {
        for field in ["triple", "installed", "buildable", "reasons", "checked"] {
            assert!(
                target.get(field).is_some(),
                "target missing key '{field}': {target}"
            );
        }
        assert!(target["reasons"].is_array());
    }

    // Providers matrix: every entry has the configured/reachable/probed flags
    // plus an errors[] array.
    let providers = data["providers"].as_array().expect("providers array");
    assert!(!providers.is_empty(), "no providers reported");
    for provider in providers {
        for field in [
            "name",
            "configured",
            "reachable",
            "latency_ms",
            "errors",
            "probed",
        ] {
            assert!(
                provider.get(field).is_some(),
                "provider missing key '{field}': {provider}"
            );
        }
        assert!(provider["errors"].is_array());
        // Default doctor invocation skips probes; reachable/latency_ms are null.
        assert_eq!(provider["probed"], false);
        assert!(provider["reachable"].is_null());
        assert!(provider["latency_ms"].is_null());
    }

    // Capability matrix: every entry has a stdlib effect name and a non-empty
    // sandbox-profile list.
    let capabilities = data["capabilities"].as_array().expect("capabilities array");
    assert!(!capabilities.is_empty(), "no capabilities reported");
    for capability in capabilities {
        assert!(capability["name"].is_string());
        let profiles = capability["available_in_sandbox_profile"]
            .as_array()
            .expect("sandbox profile list");
        assert!(
            !profiles.is_empty(),
            "capability {capability} should list at least one profile"
        );
    }

    let checks = data["checks"].as_array().expect("checks array");
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

/// `doctor` is registered in the top-level `--json-schemas` catalog so
/// agents can discover the JSON contract before invoking the command.
/// Both the catalog envelope and the doctor payload conform to the same
/// `JsonEnvelope` shape — this test is the source of truth for that
/// invariant.
#[test]
fn doctor_appears_in_json_schemas_catalog() {
    let output = Command::new(binary_path())
        .args(["--json-schemas", "--command", "doctor"])
        .output()
        .expect("spawn harn --json-schemas --command doctor");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("catalog is valid JSON");
    let data = assert_envelope(&parsed, CATALOG_SCHEMA_VERSION);
    let entries = data.as_array().expect("data is an array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["command"], "doctor");
    assert_eq!(entries[0]["schemaVersion"], DOCTOR_SCHEMA_VERSION);
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
        .args(["doctor", "--json"])
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
