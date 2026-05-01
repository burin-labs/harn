//! In-process tests for the `harn crystallize` library API
//! (`harn_vm::orchestration::*`). Replaces the prior subprocess-spawning
//! version that ran the `harn` binary per case (#1067).

use std::fs;
use std::path::Path;

use harn_vm::orchestration::{
    build_crystallization_bundle, crystallize_traces, load_crystallization_traces_from_dir,
    shadow_replay_bundle, validate_crystallization_bundle, write_crystallization_artifacts,
    write_crystallization_bundle, BundleOptions, CrystallizationArtifacts, CrystallizeOptions,
};
use serde_json::{json, Value};
use tempfile::TempDir;

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

/// Run the same pipeline as `harn crystallize <flags>` (the `mine`
/// path). Returns `(report_path, bundle_dir)` so callers can inspect
/// on-disk artifacts.
struct MineOutcome {
    artifacts: CrystallizationArtifacts,
    workflow_path: std::path::PathBuf,
    report_path: std::path::PathBuf,
    eval_pack_path: Option<std::path::PathBuf>,
    bundle_dir: std::path::PathBuf,
}

fn mine(
    temp: &TempDir,
    traces_dir: &Path,
    workflow_name: &str,
    package_name: Option<&str>,
    bundle_team: Option<&str>,
    bundle_repo: Option<&str>,
    eval_pack: bool,
    min_examples: usize,
) -> MineOutcome {
    let traces = load_crystallization_traces_from_dir(traces_dir).unwrap();
    let normalized = traces.clone();
    let eval_pack_path = if eval_pack {
        Some(temp.path().join(format!("{workflow_name}.harn.eval.toml")))
    } else {
        None
    };
    let artifacts = crystallize_traces(
        traces,
        CrystallizeOptions {
            min_examples,
            workflow_name: Some(workflow_name.to_string()),
            package_name: package_name.map(str::to_string),
            author: None,
            approver: None,
            eval_pack_link: eval_pack_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        },
    )
    .unwrap();

    let bundle_dir = temp.path().join("bundle");
    let bundle = build_crystallization_bundle(
        artifacts.clone(),
        &normalized,
        BundleOptions {
            external_key: None,
            title: None,
            team: bundle_team.map(str::to_string),
            repo: bundle_repo.map(str::to_string),
            risk_level: None,
            rollout_policy: None,
        },
    )
    .unwrap();

    let workflow_path = temp.path().join(format!("{workflow_name}.harn"));
    let report_path = temp.path().join("report.json");
    write_crystallization_artifacts(
        artifacts.clone(),
        &workflow_path,
        &report_path,
        eval_pack_path.as_deref(),
    )
    .unwrap();
    write_crystallization_bundle(&bundle, &bundle_dir).unwrap();

    MineOutcome {
        artifacts,
        workflow_path,
        report_path,
        eval_pack_path,
        bundle_dir,
    }
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

    let outcome = mine(
        &temp,
        &traces_dir,
        "version_bump",
        Some("release-workflows"),
        Some("platform"),
        Some("burin-labs/harn"),
        true,
        5,
    );
    assert!(
        outcome.workflow_path.exists(),
        "workflow file missing at {}",
        outcome.workflow_path.display()
    );
    assert!(outcome.report_path.exists());
    assert!(outcome.eval_pack_path.as_ref().unwrap().exists());

    // Manifest sanity check: schema marker, fixture redaction, plan-vs-candidate kind.
    let manifest_path = outcome.bundle_dir.join("candidate.json");
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

    // The validate library API succeeds with zero problems.
    let validation = validate_crystallization_bundle(&outcome.bundle_dir).unwrap();
    assert!(
        validation.is_ok(),
        "validation problems: {:#?}",
        validation.problems
    );

    // Shadow replay also passes against the bundle's own redacted fixtures.
    let (shadow_manifest, shadow) = shadow_replay_bundle(&outcome.bundle_dir).unwrap();
    assert!(shadow.pass, "shadow failures: {:#?}", shadow.failures);
    assert_eq!(
        serde_json::to_value(&shadow_manifest.kind).unwrap(),
        manifest["kind"]
    );
    let _ = outcome.artifacts;
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

    let outcome = mine(
        &temp,
        &traces_dir,
        "linear_triage_plan",
        None,
        None,
        None,
        false,
        3,
    );

    let manifest: Value =
        serde_json::from_slice(&fs::read(outcome.bundle_dir.join("candidate.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["kind"], json!("plan_only"));
    assert_eq!(manifest["risk_level"], json!("low"));

    let validation = validate_crystallization_bundle(&outcome.bundle_dir).unwrap();
    assert!(
        validation.is_ok(),
        "validation problems: {:#?}",
        validation.problems
    );
}
