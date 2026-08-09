//! `harn doctor` dispatch contract tests.
//!
//! The host gathers toolchain, provider, manifest, capability,
//! hardware, Ollama, and target-probe data into a structured report.
//! Rendering lives in `crates/harn-stdlib/src/stdlib/cli/doctor.harn`.

use std::collections::HashSet;

use crate::test_util;

use test_util::process::{harn_e2e_command, run_harn_e2e as run};

fn parse_json(s: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|err| {
        panic!("{label} stdout is not valid JSON: {err}\n--- payload ---\n{s}")
    })
}

#[test]
fn doctor_human_text_renders_core_sections() {
    let outcome = run(&["doctor"], &[]);
    assert!(
        [0, 1].contains(&outcome.exit_code),
        "unexpected doctor exit code {}; stderr={}",
        outcome.exit_code,
        outcome.stderr
    );
    for needle in [
        "Harn doctor",
        "--- Targets ---",
        "--- Providers ---",
        "--- Stdlib capabilities ---",
        "--- Summary ---",
        "--- Next step ---",
    ] {
        assert!(
            outcome.stdout.contains(needle),
            "doctor stdout missing {needle:?}:\n{}",
            outcome.stdout
        );
    }
}

#[test]
fn doctor_json_envelope_parses() {
    let outcome = run(&["doctor", "--json"], &[]);
    assert!(
        [0, 1].contains(&outcome.exit_code),
        "unexpected doctor --json exit code {}; stderr={}",
        outcome.exit_code,
        outcome.stderr
    );
    let value = parse_json(&outcome.stdout, "doctor --json");
    assert_eq!(value["schemaVersion"], 2);
    assert_eq!(value["ok"], true);
}

/// The `--json` envelope must carry the canonical doctor schema
/// version and the top-level structure agents rely on (`host`,
/// `targets`, `providers`, `checks`, `summary`, `next_step`). This
/// guards against a renderer change that silently drops a top-level
/// field — the byte-identity test would catch a re-ordering, but a
/// silent regression in the report struct itself needs an explicit
/// shape assertion.
#[test]
fn doctor_json_envelope_carries_schema_and_top_level_keys() {
    let outcome = run(&["doctor", "--json"], &[]);
    let value = parse_json(&outcome.stdout, "doctor --json");
    assert_eq!(
        value["schemaVersion"], 2,
        "doctor schema version drifted; bump DOCTOR_SCHEMA_VERSION + downstream consumers"
    );
    assert_eq!(value["ok"], true, "doctor --json should have ok=true");
    let data = &value["data"];
    let required_keys: HashSet<&str> = [
        "host",
        "providers_config_path",
        "model_defaults",
        "targets",
        "providers",
        "capabilities",
        "checks",
        "summary",
        "hardware",
        "next_step",
    ]
    .into_iter()
    .collect();
    let actual_keys: HashSet<&str> = data
        .as_object()
        .expect("doctor data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    for key in &required_keys {
        assert!(
            actual_keys.contains(key),
            "doctor data missing required key '{key}' (actual: {actual_keys:?})"
        );
    }
    // Spot-check the substructures so a future renderer change can't
    // silently flatten `host` into the top level.
    assert!(data["host"]["os"].is_string(), "host.os should be string");
    assert!(
        data["host"]["arch"].is_string(),
        "host.arch should be string"
    );
    assert!(
        data["summary"]["ok"].is_number(),
        "summary.ok should be a number"
    );
    assert!(
        data["checks"].is_array(),
        "checks should be an array of check rows"
    );
}

/// Run `harn doctor` against an empty temp dir to exercise the
/// "no manifest / no skills / no metadata" renderer path.
#[test]
fn doctor_in_empty_dir_renders_no_manifest_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_string_lossy().into_owned();

    let mut cmd = harn_e2e_command();
    cmd.arg("doctor").current_dir(&cwd);
    for key in ["NO_COLOR", "HARN_COLOR"] {
        cmd.env_remove(key);
    }
    let output = cmd.output().expect("spawn harn");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no harn.toml found"),
        "empty-dir doctor output should mention missing manifest:\n{stdout}"
    );
}
