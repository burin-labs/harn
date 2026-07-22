//! End-to-end coverage for `harn workflow patch`, `harn workflow function-tools`,
//! and `harn workflow nested-ceiling` JSON surfaces.

use std::path::PathBuf;
use std::process::Command;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(path: &str) -> PathBuf {
    manifest_dir().join("../..").join(path)
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
fn workflow_patch_validate_emits_structured_diff_and_capability_delta() {
    let bundle = fixture("docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json");
    let patch = fixture("docs/fixtures/workflow-bundles/pr-monitor-verifier.patch.json");
    let output = run_harn(&[
        "workflow",
        "patch",
        "validate",
        "--bundle",
        bundle.to_str().unwrap(),
        "--patch",
        patch.to_str().unwrap(),
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let report = stdout_json(&output);
    assert_eq!(report["valid"], serde_json::json!(true));
    assert_eq!(report["patch_id"], "pr-monitor-verifier-001");
    let added = report["graph_diff"]["added_nodes"].as_array().unwrap();
    let added_strings: Vec<String> = added
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    assert!(added_strings.contains(&"verify_logs".to_string()));
    assert!(added_strings.contains(&"repair_logs".to_string()));
    assert!(report["bundle_validation"]["valid"]
        .as_bool()
        .unwrap_or(false));
    assert!(report["capability_delta"]["widening"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn workflow_patch_apply_writes_valid_bundle() {
    let bundle = fixture("docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json");
    let patch = fixture("docs/fixtures/workflow-bundles/pr-monitor-verifier.patch.json");
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("patched.bundle.json");
    let output = run_harn(&[
        "workflow",
        "patch",
        "apply",
        "--bundle",
        bundle.to_str().unwrap(),
        "--patch",
        patch.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let report = stdout_json(&output);
    assert_eq!(report["valid"], serde_json::json!(true));
    let validate = run_harn(&[
        "workflow",
        "validate",
        "--bundle",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert!(validate.status.success());
    let validation = stdout_json(&validate);
    assert_eq!(validation["valid"], serde_json::json!(true));
}

#[test]
fn workflow_patch_validate_rejects_widening_against_parent() {
    let bundle = fixture("docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json");
    let temp = tempfile::tempdir().unwrap();
    let patch_path = temp.path().join("widening.patch.json");
    std::fs::write(
        &patch_path,
        r#"{
            "schema_version": 1,
            "id": "patch-widen-autonomy",
            "operations": [
                {"op": "update_bundle_policy", "policy": {"autonomy_tier": "act_auto"}}
            ]
        }"#,
    )
    .unwrap();
    let parent_path = temp.path().join("parent.json");
    std::fs::write(
        &parent_path,
        r#"{
            "tools": [],
            "capabilities": {
                "workspace": ["read_text", "list"],
                "connector": ["call"],
                "process": ["exec"]
            },
            "side_effect_level": "process_exec"
        }"#,
    )
    .unwrap();
    let output = run_harn(&[
        "workflow",
        "patch",
        "validate",
        "--bundle",
        bundle.to_str().unwrap(),
        "--patch",
        patch_path.to_str().unwrap(),
        "--parent-ceiling",
        parent_path.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        !output.status.success(),
        "expected non-zero exit: {output:?}"
    );
    let report = stdout_json(&output);
    assert_eq!(report["valid"], serde_json::json!(false));
    let kinds: Vec<String> = report["capability_delta"]["widening"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"autonomy_tier".to_string()));
}

#[test]
fn workflow_function_tools_lists_safe_descriptors() {
    let output = run_harn(&["workflow", "function-tools", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let descriptors = stdout_json(&output);
    let array = descriptors.as_array().expect("function-tools is an array");
    assert!(!array.is_empty());
    for descriptor in array {
        let kind = descriptor["annotations"]["kind"].as_str().unwrap();
        assert!(
            matches!(kind, "read" | "search" | "think" | "fetch"),
            "non-read kind in registry: {kind}"
        );
        let level = descriptor["annotations"]["side_effect_level"]
            .as_str()
            .unwrap();
        assert!(
            matches!(level, "none" | "read_only"),
            "non-readonly: {level}"
        );
    }
    let names: Vec<String> = array
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"workflow_patch_validate".to_string()));
    assert!(names.contains(&"workflow_bundle_validate".to_string()));
    assert!(names.contains(&"workflow_bundle_preview".to_string()));
}

#[test]
fn skill_pack_pr_monitor_verifier_patch_validates_against_recipe_bundle() {
    let bundle = manifest_dir()
        .join("../..")
        .join("examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json");
    let patch = manifest_dir().join("../..").join(
        "examples/skill-packs/workflow-authoring/recipes/pr-monitor-verifier-patch/patch.json",
    );
    let parent = manifest_dir()
        .join("../..")
        .join("docs/fixtures/workflow-bundles/parent-act-with-approval.policy.json");

    let output = run_harn(&[
        "workflow",
        "patch",
        "validate",
        "--bundle",
        bundle.to_str().unwrap(),
        "--patch",
        patch.to_str().unwrap(),
        "--parent-ceiling",
        parent.to_str().unwrap(),
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let report = stdout_json(&output);
    assert_eq!(report["valid"], serde_json::json!(true));
    assert!(report["capability_delta"]["widening"]
        .as_array()
        .unwrap()
        .is_empty());
    let added: Vec<String> = report["graph_diff"]["added_nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(added.contains(&"verify_logs".to_string()));
    assert!(added.contains(&"repair_logs".to_string()));
}

#[test]
fn workflow_nested_ceiling_rejects_act_auto_under_read_only_parent() {
    let bundle = fixture("docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json");
    let temp = tempfile::tempdir().unwrap();
    let parent_path = temp.path().join("ro-parent.json");
    std::fs::write(
        &parent_path,
        r#"{
            "tools": [],
            "capabilities": {
                "workspace": ["read_text", "list", "exists"]
            },
            "side_effect_level": "read_only"
        }"#,
    )
    .unwrap();
    let output = run_harn(&[
        "workflow",
        "nested-ceiling",
        "--bundle",
        bundle.to_str().unwrap(),
        "--parent",
        parent_path.to_str().unwrap(),
        "--json",
    ]);
    assert!(!output.status.success(), "{output:?}");
    let report = stdout_json(&output);
    assert!(!report["violations"].as_array().unwrap().is_empty());
}
