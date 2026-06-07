use super::*;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn minimal_run(status: &str) -> RunRecord {
    RunRecord {
        type_name: "workflow_run".to_string(),
        id: "run_1".to_string(),
        workflow_id: "workflow_1".to_string(),
        status: status.to_string(),
        usage: Some(LlmUsageRecord {
            total_duration_ms: 12,
            total_cost: 0.01,
            input_tokens: 3,
            output_tokens: 4,
            call_count: 1,
            models: vec!["mock".to_string()],
        }),
        replay_fixture: Some(ReplayFixture {
            type_name: "replay_fixture".to_string(),
            expected_status: "completed".to_string(),
            ..ReplayFixture::default()
        }),
        ..RunRecord::default()
    }
}

#[test]
fn eval_pack_manifest_toml_runs_replay_case() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(
        &run_path,
        serde_json::to_string(&minimal_run("completed")).unwrap(),
    )
    .unwrap();
    let pack_path = temp.path().join("harn.eval.toml");
    fs::write(
        &pack_path,
        r#"
version = 1
id = "connector-regressions"
name = "Connector regressions"

[[cases]]
id = "webhook"
name = "Webhook normalization"
run = "run.json"
rubrics = ["status"]

[[rubrics]]
id = "status"
kind = "deterministic"

[[rubrics.assertions]]
kind = "run-status"
expected = "completed"
"#,
    )
    .unwrap();

    let manifest = load_eval_pack_manifest(&pack_path).unwrap();
    let report = evaluate_eval_pack_manifest(&manifest).unwrap();

    assert!(report.pass);
    assert_eq!(report.total, 1);
    assert_eq!(report.cases[0].label, "Webhook normalization");
}

#[test]
fn eval_pack_trials_split_and_stats_rows() {
    let temp = tempfile::tempdir().unwrap();
    let pass_path = temp.path().join("pass.json");
    fs::write(
        &pass_path,
        serde_json::to_string(&minimal_run("completed")).unwrap(),
    )
    .unwrap();
    let fail_path = temp.path().join("fail.json");
    fs::write(
        &fail_path,
        serde_json::to_string(&minimal_run("failed")).unwrap(),
    )
    .unwrap();
    let pack_path = temp.path().join("harn.eval.toml");
    fs::write(
        &pack_path,
        r#"
version = 1
id = "trial-pack"
trials = 3

[split]
tune = ["pass-case"]
holdout = ["fail-case"]

[[cases]]
id = "pass-case"
run = "pass.json"
rubrics = ["status"]

[[cases]]
id = "fail-case"
run = "fail.json"
rubrics = ["status"]

[[rubrics]]
id = "status"
kind = "deterministic"

[[rubrics.assertions]]
kind = "run-status"
expected = "completed"
"#,
    )
    .unwrap();

    let manifest = load_eval_pack_manifest(&pack_path).unwrap();
    let split = validate_eval_pack_split(&manifest).unwrap();
    assert_eq!(split.covered_count, 2);
    assert_eq!(manifest.trials, 3);
    assert!(manifest.cases[0].case_fingerprint.len() >= 16);

    let report = evaluate_eval_pack_manifest(&manifest).unwrap();

    assert!(!report.pass);
    assert_eq!(report.trial_count, 6);
    assert_eq!(report.stats_rows.len(), 2);
    assert_eq!(report.cases[0].trial_count, 3);
    assert_eq!(report.cases[0].split.as_deref(), Some("tune"));
    assert_eq!(report.cases[0].reliability.status, "all-pass");
    assert_eq!(report.cases[0].stats_row.passes, 3);
    assert_eq!(report.cases[1].split.as_deref(), Some("holdout"));
    assert_eq!(report.cases[1].reliability.status, "all-fail");
    assert_eq!(report.cases[1].stats_row.fails, 3);
    assert_eq!(report.stats.macro_pass_at_1, 0.5);
}

#[test]
fn eval_pack_split_validation_rejects_duplicate_overlap_unknown_and_missing() {
    let pack = serde_json::json!({
        "id": "bad-split",
        "split": {
            "tune": ["a", "a", "b"],
            "holdout": ["b", "ghost"]
        },
        "cases": [
            {"id": "a", "run": "a.json"},
            {"id": "b", "run": "b.json"},
            {"id": "c", "run": "c.json"}
        ]
    });
    let manifest =
        normalize_eval_pack_manifest_value(&crate::stdlib::json_to_vm_value(&pack)).unwrap();
    let error = validate_eval_pack_split(&manifest).unwrap_err();
    let message = error.to_string();

    assert!(message.contains("duplicate partition entries: tune:a"));
    assert!(message.contains("overlapping cases:"));
    assert!(message.contains("b:"));
    assert!(message.contains("holdout"));
    assert!(message.contains("tune"));
    assert!(message.contains("unknown cases: holdout:ghost"));
    assert!(message.contains("missing cases: c"));
}

#[test]
fn eval_pack_case_fingerprint_is_stable_and_verification_sensitive() {
    let base = serde_json::json!({
        "id": "fingerprints",
        "fixtures": [
            {
                "id": "fixture",
                "kind": "replay",
                "inline": {
                    "_type": "replay_fixture",
                    "expected_status": "completed",
                    "stage_assertions": []
                }
            }
        ],
        "rubrics": [
            {
                "id": "status",
                "kind": "deterministic",
                "assertions": [{"kind": "run-status", "expected": "completed"}]
            }
        ],
        "cases": [{"id": "case", "run": "run.json", "fixture": "fixture", "rubrics": ["status"]}]
    });
    let changed = serde_json::json!({
        "id": "fingerprints",
        "fixtures": [
            {
                "id": "fixture",
                "kind": "replay",
                "inline": {
                    "_type": "replay_fixture",
                    "expected_status": "completed",
                    "stage_assertions": []
                }
            }
        ],
        "rubrics": [
            {
                "id": "status",
                "kind": "deterministic",
                "assertions": [{"kind": "run-status", "expected": "failed"}]
            }
        ],
        "cases": [{"id": "case", "run": "run.json", "fixture": "fixture", "rubrics": ["status"]}]
    });
    let base_manifest =
        normalize_eval_pack_manifest_value(&crate::stdlib::json_to_vm_value(&base)).unwrap();
    let same_manifest =
        normalize_eval_pack_manifest_value(&crate::stdlib::json_to_vm_value(&base)).unwrap();
    let changed_manifest =
        normalize_eval_pack_manifest_value(&crate::stdlib::json_to_vm_value(&changed)).unwrap();

    assert_eq!(
        base_manifest.cases[0].case_fingerprint,
        same_manifest.cases[0].case_fingerprint
    );
    assert_ne!(
        base_manifest.cases[0].case_fingerprint,
        changed_manifest.cases[0].case_fingerprint
    );
}

#[test]
fn eval_pack_warning_case_does_not_block() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(
        &run_path,
        serde_json::to_string(&minimal_run("completed")).unwrap(),
    )
    .unwrap();
    let pack_path = temp.path().join("harn.eval.toml");
    fs::write(
        &pack_path,
        r#"
version = 1
id = "budgets"

[[cases]]
id = "latency-budget"
run = "run.json"
severity = "warning"

[cases.thresholds]
max-latency-ms = 1
"#,
    )
    .unwrap();

    let manifest = load_eval_pack_manifest(&pack_path).unwrap();
    let report = evaluate_eval_pack_manifest(&manifest).unwrap();

    assert!(report.pass);
    assert_eq!(report.warning_failed, 1);
    assert!(report.cases[0].warnings[0].contains("latency"));
}

#[test]
fn eval_pack_manifest_runs_persona_ladder() {
    let temp = tempfile::tempdir().unwrap();
    let pack_path = temp.path().join("harn.eval.toml");
    let base_dir = format!("{:?}", repo_root().display().to_string());
    let artifact_root = format!("{:?}", temp.path().join("artifacts").display().to_string());
    fs::write(
        &pack_path,
        format!(
            r#"
version = 1
id = "merge-captain-ladders"
base_dir = {base_dir}

[[ladders]]
id = "merge-captain-timeout"
persona = "merge_captain"
artifact-root = {artifact_root}

[ladders.backend]
kind = "replay"
path = "examples/personas/merge_captain/transcripts/green_pr.jsonl"

[[ladders.model-routes]]
id = "gemma-value"
route = "local/gemma-value"
provider = "llama.cpp"
model = "gemma"
profile = "value"

[[ladders.timeout-tiers]]
id = "tiny"
max-tool-calls = 1

[[ladders.timeout-tiers]]
id = "balanced"
max-tool-calls = 4
max-model-calls = 1
"#
        ),
    )
    .unwrap();

    let manifest = load_eval_pack_manifest(&pack_path).unwrap();
    let report = evaluate_eval_pack_manifest(&manifest).unwrap();

    assert!(report.pass);
    assert_eq!(report.total, 1);
    assert_eq!(report.ladders.len(), 1);
    assert_eq!(
        report.ladders[0].first_correct_tier.as_deref(),
        Some("balanced")
    );
    assert_eq!(report.ladders[0].tiers[0].outcome, "degraded");
    assert_eq!(report.ladders[0].tiers[1].outcome, "correct");
}

#[test]
fn eval_pack_manifest_runs_friction_context_pack_case() {
    let temp = tempfile::tempdir().unwrap();
    let events_path = temp.path().join("incident-friction.json");
    fs::write(
        &events_path,
        r#"
{
  "events": [
{
  "kind": "repeated_query",
  "source": "incident-triage",
  "actor": "sre",
  "tool": "splunk",
  "provider": "splunk",
  "redacted_summary": "Checkout incidents need the same Splunk search",
  "recurrence_hints": ["checkout incident queries"],
  "estimated_time_ms": 300000,
  "metadata": {
    "query": "index=checkout service=api error",
    "capability": "splunk.search",
    "secret_ref": "SPLUNK_READ_TOKEN",
    "output_slot": "splunk_errors"
  }
},
{
  "kind": "repeated_query",
  "source": "incident-triage",
  "actor": "sre",
  "tool": "splunk",
  "provider": "splunk",
  "redacted_summary": "Checkout incident triage repeated the Splunk search",
  "recurrence_hints": ["checkout incident queries"],
  "estimated_time_ms": 240000,
  "metadata": {
    "query": "index=checkout service=api error",
    "capability": "splunk.search",
    "secret_ref": "SPLUNK_READ_TOKEN",
    "output_slot": "splunk_errors"
  }
}
  ]
}
"#,
    )
    .unwrap();
    let pack_path = temp.path().join("harn.eval.toml");
    fs::write(
        &pack_path,
        r#"
version = 1
id = "team-learning"
name = "Team learning evals"

[[fixtures]]
id = "incident-friction"
kind = "friction-events"
path = "incident-friction.json"

[[cases]]
id = "incident-context-pack"
name = "Incident context pack suggestion"
friction_events = "incident-friction"
rubrics = ["context-pack"]

[[rubrics]]
id = "context-pack"
kind = "friction"

[[rubrics.assertions]]
kind = "context-pack-suggestion"
contains = "incident"
expected = { min_suggestions = 1, recommended_artifact = "context_pack", required_capability = "splunk.search", required_output_slot = "splunk_errors" }
"#,
    )
    .unwrap();

    let manifest = load_eval_pack_manifest(&pack_path).unwrap();
    let report = evaluate_eval_pack_manifest(&manifest).unwrap();

    assert!(report.pass);
    assert_eq!(report.total, 1);
    assert_eq!(report.cases[0].run_id, "friction_events");
    assert_eq!(report.cases[0].stage_count, 2);
}
