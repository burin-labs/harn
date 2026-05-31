use std::fs;
use std::path::Path;

use harn_cli::cli::EvalSkillGateArgs;
use sha2::{Digest, Sha256};

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn skill_gate_cli_writes_report_and_receipt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("out");
    let grader = "stable grader\n";
    write(&tmp.path().join("grader/check.txt"), grader);
    write(
        &tmp.path().join("skills/good/SKILL.md"),
        "Use the post-cutoff API name and keep the answer scoped.\n",
    );
    let manifest = tmp.path().join("manifest.json");
    fs::write(
        &manifest,
        format!(
            r#"{{
  "_type": "harn.skill_gate.manifest.v1",
  "version": 1,
  "id": "skill-gate-cli",
  "target_model": {{"id": "mock-cheap", "knowledge_cutoff": "2026-05-01", "context_budget_tokens": 220}},
  "policy": {{"min_included_tasks": 1, "min_score_lift": 0.1, "min_gap_recovery": 0.25, "max_regression_rate": 0.0, "max_context_delta_tokens": 120}},
  "grader": {{"id": "immutable", "immutable_paths": [{{"path": "grader/check.txt", "sha256": "{}"}}]}},
  "tasks": [
    {{"id": "post-cutoff", "cluster": "api-drift", "heldout": {{"kind": "post_cutoff", "created_at": "2026-05-20"}}, "baseline_score": 0.2, "frontier_score": 1.0, "baseline_passed": false}}
  ],
  "variants": [
    {{"id": "known-good", "candidate": {{"kind": "skill", "paths": ["skills/good/SKILL.md"]}}, "case_results": [{{"task_id": "post-cutoff", "score": 0.8, "passed": true}}]}}
  ]
}}"#,
            sha256(grader)
        ),
    )
    .unwrap();

    let exit = harn_cli::commands::eval_skill_gate::run(EvalSkillGateArgs {
        manifest,
        output: Some(output.clone()),
        json: false,
    })
    .await;
    assert_eq!(exit, 0);

    let summary_raw = fs::read_to_string(output.join("summary.json")).expect("summary exists");
    let summary: serde_json::Value = serde_json::from_str(&summary_raw).expect("summary json");
    assert_eq!(summary["_type"], "harn.skill_gate.report.v1");
    assert_eq!(summary["pass"], true);
    assert_eq!(summary["selected_variant_id"], "known-good");

    let receipt_raw = fs::read_to_string(output.join("receipt.json")).expect("receipt exists");
    let receipt: serde_json::Value = serde_json::from_str(&receipt_raw).expect("receipt json");
    assert_eq!(receipt["_type"], "harn.skill_gate.receipt.v1");
    assert_eq!(receipt["accepted"], true);
    assert!(output.join("per_case.jsonl").exists());
    assert!(output.join("summary.md").exists());
}
