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
