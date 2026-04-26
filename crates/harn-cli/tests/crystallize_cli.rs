use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{json, Value};
use tempfile::TempDir;

fn run_harn(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to spawn harn binary")
}

fn write_trace(dir: &Path, name: &str, payload: &Value) {
    let path = dir.join(name);
    fs::write(&path, serde_json::to_vec_pretty(payload).unwrap()).unwrap();
}

fn version_bump_trace(idx: usize) -> Value {
    let version = format!("0.7.{idx}");
    json!({
        "version": 1,
        "id": format!("trace_release_{idx}"),
        "actions": [
            {
                "id": format!("trace_release_{idx}-checkout"),
                "kind": "tool_call",
                "name": "git.checkout_branch",
                "parameters": {
                    "repo_path": format!("/work/harn-{idx}"),
                    "branch_name": format!("release-{version}")
                },
                "side_effects": [
                    {"kind": "git_ref", "target": "release-branch", "capability": "git.write"}
                ],
                "capabilities": ["git.write"],
                "deterministic": true
            },
            {
                "id": format!("trace_release_{idx}-manifest"),
                "kind": "file_mutation",
                "name": "update_manifest_version",
                "parameters": {"version": version, "path": "harn.toml"},
                "inputs": {"path": "harn.toml", "version": version},
                "side_effects": [
                    {"kind": "file_write", "target": "harn.toml", "capability": "fs.write"}
                ],
                "capabilities": ["fs.write"],
                "deterministic": true
            },
            {
                "id": format!("trace_release_{idx}-release"),
                "kind": "tool_call",
                "name": "prepare_release_notes",
                "parameters": {
                    "release_target": "crates.io",
                    "version": version
                },
                "deterministic": true
            }
        ]
    })
}

fn plan_only_trace(idx: usize) -> Value {
    json!({
        "version": 1,
        "id": format!("trace_plan_{idx}"),
        "actions": [
            {
                "id": format!("trace_plan_{idx}-classify"),
                "kind": "tool_call",
                "name": "classify_issue",
                "parameters": {
                    "issue_id": format!("HAR-{idx}"),
                    "team_key": "HAR"
                },
                "capabilities": ["linear.read"],
                "deterministic": true
            },
            {
                "id": format!("trace_plan_{idx}-receipt"),
                "kind": "receipt_write",
                "name": "emit_receipt",
                "parameters": {"kind": "plan", "summary": format!("plan-only #{idx}")},
                "side_effects": [
                    {
                        "kind": "receipt_write",
                        "target": "tenant_event_log",
                        "capability": "receipt.write"
                    }
                ],
                "capabilities": ["receipt.write"],
                "deterministic": true
            }
        ]
    })
}

#[test]
fn crystallize_version_bump_emits_validatable_bundle() {
    let temp = TempDir::new().unwrap();
    let traces_dir = temp.path().join("traces");
    fs::create_dir_all(&traces_dir).unwrap();
    for idx in 0..5 {
        write_trace(
            &traces_dir,
            &format!("release_{idx}.json"),
            &version_bump_trace(idx),
        );
    }
    let workflow_path = temp.path().join("version_bump.harn");
    let report_path = temp.path().join("report.json");
    let eval_pack_path = temp.path().join("version_bump.harn.eval.toml");
    let bundle_dir = temp.path().join("bundle");

    let mine = run_harn(
        temp.path(),
        &[
            "crystallize",
            "--from",
            traces_dir.to_str().unwrap(),
            "--out",
            workflow_path.to_str().unwrap(),
            "--report",
            report_path.to_str().unwrap(),
            "--eval-pack",
            eval_pack_path.to_str().unwrap(),
            "--bundle",
            bundle_dir.to_str().unwrap(),
            "--workflow-name",
            "version_bump",
            "--package-name",
            "release-workflows",
            "--bundle-team",
            "platform",
            "--bundle-repo",
            "burin-labs/harn",
            "--min-examples",
            "5",
        ],
    );
    assert!(
        mine.status.success(),
        "mine failed: stdout={} stderr={}",
        String::from_utf8_lossy(&mine.stdout),
        String::from_utf8_lossy(&mine.stderr),
    );

    // Manifest sanity check: schema marker, fixture redaction, plan-vs-candidate kind.
    let manifest_path = bundle_dir.join("candidate.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(
        manifest["schema"],
        Value::String("harn.crystallization.candidate.bundle".to_string())
    );
    assert_eq!(manifest["schema_version"], json!(1));
    assert_eq!(manifest["kind"], json!("candidate"));
    assert_eq!(manifest["external_key"], json!("version-bump"));
    let workflow = &manifest["workflow"];
    assert_eq!(workflow["name"], json!("version_bump"));
    assert_eq!(workflow["package_name"], json!("release-workflows"));
    assert_eq!(workflow["path"], json!("workflow.harn"));
    assert_eq!(manifest["team"], json!("platform"));
    let fixtures = manifest["fixtures"].as_array().unwrap();
    assert_eq!(fixtures.len(), 5);
    assert!(fixtures
        .iter()
        .all(|fixture| fixture["redacted"] == json!(true)));

    // The validate subcommand exits 0 and reports OK.
    let validate = run_harn(
        temp.path(),
        &["crystallize", "validate", bundle_dir.to_str().unwrap()],
    );
    assert!(
        validate.status.success(),
        "validate failed: stdout={} stderr={}",
        String::from_utf8_lossy(&validate.stdout),
        String::from_utf8_lossy(&validate.stderr),
    );
    assert!(String::from_utf8_lossy(&validate.stdout).contains("OK"));

    // Shadow replay also passes against the bundle's own redacted fixtures.
    let shadow = run_harn(
        temp.path(),
        &["crystallize", "shadow", bundle_dir.to_str().unwrap()],
    );
    assert!(
        shadow.status.success(),
        "shadow failed: stdout={} stderr={}",
        String::from_utf8_lossy(&shadow.stdout),
        String::from_utf8_lossy(&shadow.stderr),
    );
    let stdout = String::from_utf8_lossy(&shadow.stdout);
    assert!(stdout.contains("pass=true"));
}

#[test]
fn crystallize_plan_only_bundle_keeps_plan_only_kind() {
    let temp = TempDir::new().unwrap();
    let traces_dir = temp.path().join("traces");
    fs::create_dir_all(&traces_dir).unwrap();
    for idx in 0..3 {
        write_trace(
            &traces_dir,
            &format!("plan_{idx}.json"),
            &plan_only_trace(idx),
        );
    }
    let workflow_path = temp.path().join("plan.harn");
    let report_path = temp.path().join("plan.report.json");
    let bundle_dir = temp.path().join("bundle");

    let mine = run_harn(
        temp.path(),
        &[
            "crystallize",
            "--from",
            traces_dir.to_str().unwrap(),
            "--out",
            workflow_path.to_str().unwrap(),
            "--report",
            report_path.to_str().unwrap(),
            "--bundle",
            bundle_dir.to_str().unwrap(),
            "--workflow-name",
            "linear_triage_plan",
            "--min-examples",
            "3",
        ],
    );
    assert!(mine.status.success(), "{:?}", mine);

    let manifest: Value =
        serde_json::from_slice(&fs::read(bundle_dir.join("candidate.json")).unwrap()).unwrap();
    assert_eq!(manifest["kind"], json!("plan_only"));
    assert_eq!(manifest["risk_level"], json!("low"));

    let validate = run_harn(
        temp.path(),
        &["crystallize", "validate", bundle_dir.to_str().unwrap()],
    );
    assert!(
        validate.status.success(),
        "validate failed: stdout={} stderr={}",
        String::from_utf8_lossy(&validate.stdout),
        String::from_utf8_lossy(&validate.stderr),
    );
}
