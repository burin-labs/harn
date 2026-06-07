use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

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

fn install_sqlite_event_log(path: PathBuf) {
    crate::event_log::reset_active_event_log();
    let log = Arc::new(crate::event_log::AnyEventLog::Sqlite(
        crate::event_log::SqliteEventLog::open(path, 128).unwrap(),
    ));
    crate::event_log::install_active_event_log(log);
}

fn ledger_row_json(case_name: &str, status: &str, trial: usize) -> serde_json::Value {
    let (passes, fails, skips) = match status {
        "PASS" => (1, 0, 0),
        "FAIL" => (0, 1, 0),
        _ => (0, 0, 1),
    };
    serde_json::json!({
        "case_name": case_name,
        "name": case_name,
        "trial": trial,
        "trials": 1,
        "status": status,
        "verification": status,
        "passes": passes,
        "fails": fails,
        "skips": skips,
    })
}

#[test]
fn eval_ledger_sqlite_backend_preserves_flat_file_semantics() {
    let temp = tempfile::tempdir().unwrap();
    install_sqlite_event_log(temp.path().join("events.sqlite"));

    let options = serde_json::json!({
        "namespace": "parity-ledger",
        "suite": "suite-a",
        "model": "mock-model",
        "commit": "commit-b",
        "branch": "main",
        "case_fingerprint": "case-fp",
        "harness_config_fingerprint": "harness-fp",
    });
    let rows = serde_json::json!([
        ledger_row_json("case-a", "PASS", 1),
        ledger_row_json("case-a", "PASS", 1),
        ledger_row_json("case-b", "skip", 1)
    ]);

    let appended = eval_ledger_append_rows_report(rows, Some(options.clone())).unwrap();

    assert_eq!(appended.appended, 3);
    assert_eq!(appended.inserted, 2);
    assert_eq!(appended.duplicates, 1);
    assert!(!appended.all_skipped);

    let read = eval_ledger_read_report(Some(options)).unwrap();

    assert_eq!(read.rows.len(), 2);
    assert_eq!(read.rows[0].case_name, "case-a");
    assert_eq!(read.rows[1].case_name, "case-b");
    assert_eq!(read.rows[0].provenance.commit, "commit-b");
    assert_eq!(read.rows[0].provenance.branch.as_deref(), Some("main"));
    assert!(read.rows[0].event_id < read.rows[1].event_id);

    let all_skip = eval_ledger_append_rows_report(
        serde_json::json!([
            ledger_row_json("case-c", "skip", 1),
            ledger_row_json("case-d", "skip", 1)
        ]),
        Some(serde_json::json!({
            "namespace": "parity-skip-ledger",
            "suite": "suite-a",
            "model": "mock-model",
            "commit": "commit-b",
            "case_fingerprint": "case-fp",
            "harness_config_fingerprint": "harness-fp",
        })),
    )
    .unwrap();

    assert!(all_skip.all_skipped);
    crate::event_log::reset_active_event_log();
}

#[test]
fn eval_ledger_prior_commit_rows_reports_mismatched_fingerprints() {
    let temp = tempfile::tempdir().unwrap();
    install_sqlite_event_log(temp.path().join("events.sqlite"));

    let rows = serde_json::json!([
        {
            "suite": "prior-pack",
            "model": "mock-model",
            "commit": "old-good",
            "case_name": "case-a",
            "name": "case-a",
            "case_fingerprint": "case-fp",
            "harness_config_fingerprint": "harness-fp",
            "trial": 1,
            "trials": 1,
            "status": "PASS",
            "verification": "PASS",
            "passes": 1
        },
        {
            "suite": "prior-pack",
            "model": "mock-model",
            "commit": "old-bad",
            "case_name": "case-a",
            "name": "case-a",
            "case_fingerprint": "case-fp",
            "harness_config_fingerprint": "stale-harness",
            "trial": 1,
            "trials": 1,
            "status": "PASS",
            "verification": "PASS",
            "passes": 1
        },
        {
            "suite": "prior-pack",
            "model": "mock-model",
            "commit": "current",
            "case_name": "case-a",
            "name": "case-a",
            "case_fingerprint": "case-fp",
            "harness_config_fingerprint": "harness-fp",
            "trial": 1,
            "trials": 1,
            "status": "PASS",
            "verification": "PASS",
            "passes": 1
        }
    ]);
    eval_ledger_append_rows_report(
        rows,
        Some(serde_json::json!({
            "namespace": "prior-ledger",
            "branch": "main"
        })),
    )
    .unwrap();

    let report = eval_ledger_prior_commit_rows_report(serde_json::json!({
        "namespace": "prior-ledger",
        "suite": "prior-pack",
        "model": "mock-model",
        "commit": "current",
        "case": "case-a",
        "case_fingerprint": "case-fp",
        "harness_config_fingerprint": "harness-fp"
    }))
    .unwrap();

    assert_eq!(report.commit.as_deref(), Some("old-good"));
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].commit, "old-good");
    assert_eq!(report.fingerprint_mismatches.len(), 1);
    assert_eq!(report.fingerprint_mismatches[0].commit, "old-bad");
    assert_eq!(
        report.fingerprint_mismatches[0].harness_config_fingerprint,
        "stale-harness"
    );
    crate::event_log::reset_active_event_log();
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

fn resumable_pack_payload() -> serde_json::Value {
    let pass_run = minimal_run("completed");
    let fail_run = minimal_run("failed");
    serde_json::json!({
        "id": "resumable-pack",
        "metadata": {
            "model": "mock-model",
            "commit": "commit-a",
            "branch": "main",
            "tool_format": "native-json",
            "pipeline_rev": "rev-a"
        },
        "fixtures": [
            {"id": "pass-run", "kind": "run-record", "inline": pass_run},
            {"id": "fail-run", "kind": "run-record", "inline": fail_run}
        ],
        "rubrics": [
            {
                "id": "status",
                "kind": "deterministic",
                "assertions": [{"kind": "run-status", "expected": "completed"}]
            }
        ],
        "cases": [
            {"id": "preseeded", "run": "fail-run", "rubrics": ["status"]},
            {"id": "fresh", "run": "pass-run", "rubrics": ["status"]}
        ]
    })
}

#[test]
fn eval_pack_resumable_skips_matching_cells_and_records_all_skip_heartbeat() {
    let temp = tempfile::tempdir().unwrap();
    install_sqlite_event_log(temp.path().join("events.sqlite"));
    let manifest = normalize_eval_pack_manifest_value(&crate::stdlib::json_to_vm_value(
        &resumable_pack_payload(),
    ))
    .unwrap();
    let harness_fingerprint = eval_pack_harness_config_fingerprint(&manifest).unwrap();

    let preseed = serde_json::json!([{
        "suite": manifest.id,
        "model": "mock-model",
        "commit": "commit-a",
        "case_name": "preseeded",
        "name": "preseeded",
        "case_fingerprint": manifest.cases[0].case_fingerprint.clone(),
        "harness_config_fingerprint": harness_fingerprint,
        "trial": 1,
        "trials": 1,
        "status": "PASS",
        "verification": "PASS",
        "passes": 1
    }]);
    eval_ledger_append_rows_report(
        preseed,
        Some(serde_json::json!({
            "namespace": "resumable-pack",
            "branch": "main"
        })),
    )
    .unwrap();

    let report = evaluate_eval_pack_manifest_resumable(&manifest, None).unwrap();

    assert!(report.pass);
    assert_eq!(report.run_state.requested_cells, 2);
    assert_eq!(report.run_state.skipped_cells, 1);
    assert_eq!(report.run_state.executed_cells, 1);
    assert_eq!(report.run_state.ledger_rows_inserted, 1);
    assert_eq!(report.cases[0].reliability.status, "all-pass");
    assert_eq!(report.cases[1].reliability.status, "all-pass");

    let rerun = evaluate_eval_pack_manifest_resumable(&manifest, None).unwrap();

    assert!(rerun.pass);
    assert!(rerun.run_state.all_skipped);
    assert_eq!(rerun.run_state.skipped_cells, 2);
    assert_eq!(rerun.run_state.executed_cells, 0);
    assert_eq!(rerun.run_state.ledger_rows_inserted, 0);
    assert!(rerun.run_state.heartbeat_event_id.is_some());
    crate::event_log::reset_active_event_log();
}

#[test]
fn eval_pack_resumable_refuses_fingerprint_mismatched_skip() {
    let temp = tempfile::tempdir().unwrap();
    install_sqlite_event_log(temp.path().join("events.sqlite"));
    let payload = {
        let mut payload = resumable_pack_payload();
        payload["cases"] = serde_json::json!([
            {"id": "preseeded", "run": "fail-run", "rubrics": ["status"]}
        ]);
        payload
    };
    let manifest =
        normalize_eval_pack_manifest_value(&crate::stdlib::json_to_vm_value(&payload)).unwrap();

    let preseed = serde_json::json!([{
        "suite": manifest.id,
        "model": "mock-model",
        "commit": "commit-a",
        "case_name": "preseeded",
        "name": "preseeded",
        "case_fingerprint": manifest.cases[0].case_fingerprint.clone(),
        "harness_config_fingerprint": "different-harness",
        "trial": 1,
        "trials": 1,
        "status": "PASS",
        "verification": "PASS",
        "passes": 1
    }]);
    eval_ledger_append_rows_report(
        preseed,
        Some(serde_json::json!({"namespace": "resumable-pack"})),
    )
    .unwrap();

    let report = evaluate_eval_pack_manifest_resumable(&manifest, None).unwrap();

    assert!(!report.pass);
    assert_eq!(report.run_state.skipped_cells, 0);
    assert_eq!(report.run_state.executed_cells, 1);
    assert_eq!(report.run_state.fingerprint_refusals, 1);
    assert_eq!(report.cases[0].reliability.status, "all-fail");
    crate::event_log::reset_active_event_log();
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
