//! End-to-end coverage for the `--json` envelopes added by epic #1753:
//! `harn lint`, `harn replay`, `harn version`, and `harn upgrade --check`.

use std::process::Command;

use harn_cli::tests::common::json_envelope::assert_envelope;

const LINT_SCHEMA_VERSION: u32 = 1;
const REPLAY_SCHEMA_VERSION: u32 = 1;
const VERSION_SCHEMA_VERSION: u32 = 1;

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
fn lint_json_emits_envelope_for_clean_and_diagnostic_files() {
    let temp = tempfile::TempDir::new().expect("tempdir");

    let clean_path = temp.path().join("clean.harn");
    std::fs::write(&clean_path, "pipeline main(task) {\n  return 1\n}\n").expect("clean write");
    let clean = Command::new(binary_path())
        .args(["lint", "--json", clean_path.to_str().unwrap()])
        .output()
        .expect("spawn harn lint --json");
    assert!(
        clean.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let parsed = stdout_json(&clean);
    let data = assert_envelope(&parsed, LINT_SCHEMA_VERSION);
    assert_eq!(data["summary"]["ok"], 1);
    assert_eq!(data["summary"]["diagnostics"], 0);
    assert_eq!(data["files"][0]["status"], "ok");

    // Unused-variable should trip the lint and surface a structured diagnostic.
    let warn_path = temp.path().join("warn.harn");
    std::fs::write(
        &warn_path,
        "pipeline main(task) {\n  let x = 1\n  return 2\n}\n",
    )
    .expect("warn write");
    let warn = Command::new(binary_path())
        .args(["lint", "--json", warn_path.to_str().unwrap()])
        .output()
        .expect("spawn harn lint --json");
    // A pure warning shouldn't fail; the envelope still carries diagnostics.
    let parsed = stdout_json(&warn);
    let data = assert_envelope(&parsed, LINT_SCHEMA_VERSION);
    assert!(
        data["summary"]["diagnostics"].as_u64().unwrap() >= 1,
        "expected at least one diagnostic, got {data}"
    );
    let diag = &data["files"][0]["diagnostics"][0];
    assert_eq!(diag["source"], "lint");
    assert!(
        diag["code"].as_str().unwrap_or("").starts_with("HARN-LNT-"),
        "lint diagnostic should carry a HARN-LNT-* code, got {diag}"
    );
    assert!(diag["span"]["start"].is_number());
}

#[test]
fn lint_json_errors_when_no_targets_match() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    // Pass a real-but-empty directory — no `.harn` or `.harn.prompt`
    // files under it. The envelope reports the structured error rather
    // than the human-readable `command_error` exit.
    let empty = temp.path().join("empty");
    std::fs::create_dir_all(&empty).expect("mkdir");
    let out = Command::new(binary_path())
        .args(["lint", "--json", empty.to_str().unwrap()])
        .output()
        .expect("spawn harn lint --json");
    assert!(
        !out.status.success(),
        "expected non-zero exit for no targets"
    );
    let parsed = stdout_json(&out);
    assert_eq!(parsed["schemaVersion"], LINT_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "no_lint_targets");
}

#[test]
fn replay_json_loads_persisted_run_record() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    // Write a minimal RunRecord shape that matches harn_vm::orchestration::RunRecord.
    let path = temp.path().join("run.json");
    let run = serde_json::json!({
        "id": "test-run-1",
        "schema_version": 1,
        "schema": "harn.run.v1",
        "status": "completed",
        "started_at": "2026-05-19T00:00:00Z",
        "completed_at": "2026-05-19T00:00:01Z",
        "stages": [
            {
                "node_id": "main",
                "status": "completed",
                "outcome": "success",
                "branch": null,
                "visible_text": "hello",
                "verification": null,
                "artifacts": []
            }
        ],
        "transitions": [],
        "transcript": { "events": [] },
        "replay_fixture": null
    });
    std::fs::write(&path, serde_json::to_string(&run).unwrap()).expect("write");

    let out = Command::new(binary_path())
        .args(["replay", "--json", path.to_str().unwrap()])
        .output()
        .expect("spawn harn replay --json");
    // We don't assert the fixture passes — synthesizing a perfectly
    // matching fixture is brittle. We only assert the envelope shape is
    // valid and that the fixture verdict is reported either way.
    let parsed = stdout_json(&out);
    assert_eq!(parsed["schemaVersion"], REPLAY_SCHEMA_VERSION);
    let data = parsed
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert!(
        !data.is_null(),
        "data should be present even on fixture-fail"
    );
    assert_eq!(data["run_id"], "test-run-1");
    assert_eq!(data["stage_count"], 1);
    assert_eq!(data["stages"][0]["node_id"], "main");
    assert!(
        data["fixture"]["pass"].is_boolean(),
        "fixture verdict should be present"
    );
}

#[test]
fn replay_json_emits_structured_error_for_missing_run_record() {
    let out = Command::new(binary_path())
        .args(["replay", "--json", "/nonexistent/path/to/run.json"])
        .output()
        .expect("spawn harn replay --json");
    assert!(!out.status.success(), "expected non-zero exit");
    let parsed = stdout_json(&out);
    assert_eq!(parsed["schemaVersion"], REPLAY_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "run_record_load_failed");
}

#[test]
fn version_json_returns_build_metadata() {
    let out = Command::new(binary_path())
        .args(["version", "--json"])
        .output()
        .expect("spawn harn version --json");
    assert!(out.status.success());
    let parsed = stdout_json(&out);
    let data = assert_envelope(&parsed, VERSION_SCHEMA_VERSION);
    assert_eq!(data["name"], "harn-cli");
    assert!(
        data["version"]
            .as_str()
            .unwrap_or("")
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false),
        "version should look like X.Y.Z, got {data}"
    );
    assert!(!data["description"].as_str().unwrap_or("").is_empty());
}

#[test]
fn json_schemas_catalog_includes_new_commands() {
    let out = Command::new(binary_path())
        .args(["--json-schemas"])
        .output()
        .expect("spawn harn --json-schemas");
    assert!(out.status.success());
    let parsed = stdout_json(&out);
    let entries = parsed["data"]
        .as_array()
        .expect("catalog data should be an array");
    let names: std::collections::BTreeSet<&str> = entries
        .iter()
        .map(|e| e["command"].as_str().unwrap_or(""))
        .collect();
    for name in ["lint", "replay", "version", "upgrade"] {
        assert!(
            names.contains(name),
            "catalog should register `{name}` (entries: {names:?})"
        );
    }
}
