//! In-process coverage of `harn crystallize` mining + bundle behavior.
//!
//! Tier 1H of the de-flake epic (#1057, #1067): the CLI command is a
//! thin wrapper around library functions in
//! `harn_vm::orchestration::{crystallize_traces,
//! build_crystallization_bundle, write_crystallization_artifacts,
//! write_crystallization_bundle, validate_crystallization_bundle,
//! shadow_replay_bundle}`. Tests call those library functions
//! directly and assert on the returned data structures plus the
//! manifest written to disk.

mod test_util;

use std::fs;
use std::path::Path;

use harn_vm::orchestration::{
    build_crystallization_bundle, crystallize_traces, ingest_release_fixture,
    load_crystallization_traces_from_dir, shadow_replay_bundle, validate_crystallization_bundle,
    write_crystallization_artifacts, write_crystallization_bundle, BundleOptions,
    CrystallizeOptions,
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

    let traces = load_crystallization_traces_from_dir(&traces_dir).expect("load traces");
    let normalized = traces.clone();
    let artifacts = crystallize_traces(
        traces,
        CrystallizeOptions {
            min_examples: 5,
            workflow_name: Some("version_bump".to_string()),
            package_name: Some("release-workflows".to_string()),
            author: None,
            approver: None,
            eval_pack_link: Some(eval_pack_path.to_string_lossy().into_owned()),
        },
    )
    .expect("crystallize");

    let bundle = build_crystallization_bundle(
        artifacts.clone(),
        &normalized,
        BundleOptions {
            external_key: None,
            title: None,
            team: Some("platform".to_string()),
            repo: Some("burin-labs/harn".to_string()),
            risk_level: None,
            rollout_policy: None,
        },
    )
    .expect("build bundle");

    let report = write_crystallization_artifacts(
        artifacts,
        &workflow_path,
        &report_path,
        Some(eval_pack_path.as_path()),
    )
    .expect("write artifacts");

    assert!(
        report.selected_candidate_id.is_some(),
        "expected a safe candidate to be selected; report: {report:#?}"
    );

    let manifest = write_crystallization_bundle(&bundle, &bundle_dir).expect("write bundle");
    assert_eq!(manifest.fixtures.len(), 5);

    // Manifest sanity check: schema marker, fixture redaction, plan-vs-candidate kind.
    let manifest_path = bundle_dir.join("candidate.json");
    let manifest_json: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(
        manifest_json["schema"],
        Value::String("harn.crystallization.candidate.bundle".to_string())
    );
    assert_eq!(manifest_json["schema_version"], json!(1));
    assert_eq!(manifest_json["kind"], json!("candidate"));
    assert_eq!(manifest_json["external_key"], json!("version-bump"));
    let workflow = &manifest_json["workflow"];
    assert_eq!(workflow["name"], json!("version_bump"));
    assert_eq!(workflow["package_name"], json!("release-workflows"));
    assert_eq!(workflow["path"], json!("workflow.harn"));
    assert_eq!(manifest_json["team"], json!("platform"));
    let fixtures = manifest_json["fixtures"].as_array().unwrap();
    assert_eq!(fixtures.len(), 5);
    assert!(fixtures
        .iter()
        .all(|fixture| fixture["redacted"] == json!(true)));

    // Bundle validation passes against itself.
    let validation = validate_crystallization_bundle(&bundle_dir).expect("validate");
    assert!(
        validation.problems.is_empty(),
        "validation reported problems: {:#?}",
        validation.problems
    );
    assert!(validation.manifest_ok);
    assert!(validation.workflow_ok);
    assert!(validation.report_ok);
    assert!(validation.fixtures_ok);
    assert!(validation.redaction_ok);

    // Shadow replay also passes against the bundle's own redacted fixtures.
    let (shadow_manifest, shadow) = shadow_replay_bundle(&bundle_dir).expect("shadow replay");
    assert!(
        shadow.pass,
        "shadow replay failed for candidate {}: {:#?}",
        shadow_manifest.candidate_id, shadow.failures
    );
    assert!(shadow.compared_traces > 0);
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

    let traces = load_crystallization_traces_from_dir(&traces_dir).expect("load traces");
    let normalized = traces.clone();
    let artifacts = crystallize_traces(
        traces,
        CrystallizeOptions {
            min_examples: 3,
            workflow_name: Some("linear_triage_plan".to_string()),
            ..Default::default()
        },
    )
    .expect("crystallize");

    let bundle =
        build_crystallization_bundle(artifacts.clone(), &normalized, BundleOptions::default())
            .expect("build bundle");

    let report = write_crystallization_artifacts(artifacts, &workflow_path, &report_path, None)
        .expect("write artifacts");
    assert!(report.selected_candidate_id.is_some());

    write_crystallization_bundle(&bundle, &bundle_dir).expect("write bundle");
    let manifest_json: Value =
        serde_json::from_slice(&fs::read(bundle_dir.join("candidate.json")).unwrap()).unwrap();
    assert_eq!(manifest_json["kind"], json!("plan_only"));
    assert_eq!(manifest_json["risk_level"], json!("low"));

    let validation = validate_crystallization_bundle(&bundle_dir).expect("validate");
    assert!(
        validation.problems.is_empty(),
        "validation reported problems: {:#?}",
        validation.problems
    );
}

#[test]
fn ingest_release_fixture_emits_validatable_bundle_with_segment_and_recovery_summary() {
    // Sample fixture lives in harn-vm. Resolve relative to harn-cli's
    // manifest dir so the test works from any cwd.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = manifest_dir
        .parent()
        .unwrap()
        .join("harn-vm/tests/fixtures/release_harn_sample");
    assert!(
        fixture_dir.exists(),
        "expected sample fixture at {}",
        fixture_dir.display()
    );

    let temp = TempDir::new().unwrap();
    let bundle_dir = temp.path().join("bundle");

    let (artifacts, fixture, trace) = ingest_release_fixture(
        &fixture_dir,
        CrystallizeOptions {
            min_examples: 1,
            workflow_name: Some("release_harn".to_string()),
            package_name: Some("release-harn".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .expect("ingest");

    // Manifest decoded with the right release identity.
    assert_eq!(fixture.manifest.release.current_version, "0.7.52");
    assert_eq!(fixture.manifest.release.next_version, "0.7.53");

    // Trace contains every event-derived action and is sorted by timestamp.
    assert!(trace.actions.len() >= 8);

    // Candidate is selected and safe to propose.
    let candidate_id = artifacts
        .report
        .selected_candidate_id
        .clone()
        .expect("selected candidate");
    let candidate = &artifacts.report.candidates[0];
    assert!(candidate.shadow.pass);

    // Plain-language splits are populated and surface review-required items.
    let segment = artifacts
        .report
        .segment_summary
        .as_ref()
        .expect("segment summary");
    assert!(segment.deterministic_count >= 4);
    assert!(segment.agentic_count >= 2);
    assert!(segment.plain_language.contains("Safe to automate"));
    assert!(segment
        .requires_human_review
        .iter()
        .any(|item| item.contains("failed step:push")));
    assert!(segment
        .requires_human_review
        .iter()
        .any(|item| item.contains("agent review")));
    assert!(segment
        .requires_human_review
        .iter()
        .any(|item| item.contains("agent recovery advice")));

    let recovery = artifacts
        .report
        .recovery_summary
        .as_ref()
        .expect("recovery summary");
    assert!(recovery.shell_failures_seen >= 1);
    assert!(recovery.recovery_advice_runs >= 1);
    assert!(recovery.failures_fed_into_agent);
    assert!(recovery.failed_steps.contains(&"push".to_string()));
    assert!(recovery.representation.contains("agent_loop"));

    // Build + write bundle, then validate + shadow-replay it.
    let bundle = build_crystallization_bundle(
        artifacts,
        std::slice::from_ref(&trace),
        BundleOptions {
            external_key: None,
            title: None,
            team: Some("merge_captain".to_string()),
            repo: Some("burin-labs/harn".to_string()),
            risk_level: None,
            rollout_policy: None,
        },
    )
    .expect("build bundle");
    let manifest = write_crystallization_bundle(&bundle, &bundle_dir).expect("write bundle");
    assert_eq!(manifest.candidate_id, candidate_id);
    assert_eq!(manifest.fixtures.len(), 1);
    assert!(manifest.fixtures[0].redacted);

    // Manifest schema marker is correct.
    let manifest_json: Value =
        serde_json::from_slice(&fs::read(bundle_dir.join("candidate.json")).unwrap()).unwrap();
    assert_eq!(
        manifest_json["schema"],
        Value::String("harn.crystallization.candidate.bundle".to_string())
    );

    // Existing validator accepts the bundle.
    let validation = validate_crystallization_bundle(&bundle_dir).expect("validate");
    assert!(
        validation.problems.is_empty(),
        "validation reported problems: {:#?}",
        validation.problems
    );

    // Shadow replay against the bundle's own redacted fixture passes.
    let (shadow_manifest, shadow) = shadow_replay_bundle(&bundle_dir).expect("shadow replay");
    assert_eq!(shadow_manifest.candidate_id, candidate_id);
    assert!(shadow.pass, "shadow replay failed: {:#?}", shadow.failures);
    assert!(shadow.compared_traces > 0);

    // Report on disk includes the segment + recovery summaries (i.e.,
    // the plain-language deterministic/agentic split is preserved
    // round-trip via the bundle, not just in-memory).
    let report_json: Value =
        serde_json::from_slice(&fs::read(bundle_dir.join("report.json")).unwrap()).unwrap();
    assert!(report_json["segment_summary"]["plain_language"]
        .as_str()
        .unwrap_or("")
        .contains("Safe to automate"));
    assert!(report_json["recovery_summary"]["failures_fed_into_agent"]
        .as_bool()
        .unwrap_or(false));
}
