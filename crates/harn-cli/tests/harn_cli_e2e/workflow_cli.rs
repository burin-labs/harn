//! End-to-end coverage for portable workflow bundle CLI JSON surfaces.

use std::path::PathBuf;
use std::process::Command;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json")
}

fn run_harn(args: &[&str]) -> std::process::Output {
    Command::new(binary_path())
        .args(args)
        .output()
        .expect("spawn harn")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

#[test]
fn workflow_bundle_validate_preview_and_run_emit_json() {
    let bundle = fixture_path();
    let bundle_arg = bundle.to_str().expect("fixture path is UTF-8");

    let validate = run_harn(&["workflow", "validate", "--bundle", bundle_arg, "--json"]);
    assert!(validate.status.success());
    let validation = stdout_json(&validate);
    assert_eq!(validation["valid"], serde_json::json!(true));
    assert_eq!(validation["bundle_id"], "github-pr-monitor");
    assert!(validation["graph_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let preview = run_harn(&["workflow", "preview", "--bundle", bundle_arg, "--json"]);
    assert!(preview.status.success());
    let preview_json = stdout_json(&preview);
    assert_eq!(preview_json["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(preview_json["triggers"].as_array().unwrap().len(), 2);
    assert_eq!(
        preview_json["validation"]["graph_digest"],
        validation["graph_digest"]
    );

    let run = run_harn(&[
        "workflow",
        "run",
        "--bundle",
        bundle_arg,
        "--trigger-id",
        "github-pr-updated",
        "--event-id",
        "github:event:43",
        "--json",
    ]);
    assert!(run.status.success());
    let receipt = stdout_json(&run);
    assert_eq!(receipt["receipt_type"], "harn.workflow_bundle.run");
    assert_eq!(receipt["status"], "completed");
    assert_eq!(receipt["executed_nodes"].as_array().unwrap().len(), 4);
    assert_eq!(receipt["event_ids"][1], "github:event:43");

    let replay = run_harn(&[
        "workflow",
        "run",
        "--bundle",
        bundle_arg,
        "--trigger-id",
        "github-pr-updated",
        "--event-id",
        "github:event:43",
        "--json",
    ]);
    assert!(replay.status.success());
    assert_eq!(receipt, stdout_json(&replay));
}
