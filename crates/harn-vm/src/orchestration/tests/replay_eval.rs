//! Replay fixtures and the eval suite built on them.
//!
//! A recorded fixture must round-trip, a failing case must be reported as failed
//! rather than swallowed, a clarifying-question eval only passes against a
//! matching HITL prompt, and a suite manifest can fail the run on a baseline
//! diff.

use crate::orchestration::*;
use std::collections::BTreeMap;
#[test]
fn replay_fixture_round_trip_passes() {
    let run = RunRecord {
        type_name: "run_record".to_string(),
        id: "run_1".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("demo".to_string()),
        task: "demo".to_string(),
        status: "completed".to_string(),
        started_at: "1".to_string(),
        finished_at: Some("2".to_string()),
        parent_run_id: None,
        root_run_id: Some("run_1".to_string()),
        stages: vec![RunStageRecord {
            id: "stage_1".to_string(),
            node_id: "act".to_string(),
            kind: "stage".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            branch: Some("success".to_string()),
            started_at: "1".to_string(),
            finished_at: Some("2".to_string()),
            visible_text: Some("done".to_string()),
            private_reasoning: None,
            transcript: None,
            verification: None,
            usage: None,
            artifacts: vec![ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "a1".to_string(),
                kind: "summary".to_string(),
                text: Some("done".to_string()),
                created_at: "1".to_string(),
                ..Default::default()
            }
            .normalize()],
            consumed_artifact_ids: vec![],
            produced_artifact_ids: vec!["a1".to_string()],
            attempts: vec![],
            metadata: BTreeMap::new(),
        }],
        transitions: vec![],
        checkpoints: vec![],
        pending_nodes: vec![],
        completed_nodes: vec!["act".to_string()],
        child_runs: vec![],
        artifacts: vec![],
        handoffs: vec![],
        policy: CapabilityPolicy::default(),
        execution: None,
        transcript: None,
        usage: None,
        replay_fixture: None,
        observability: None,
        evidence: ExecutionEvidenceRecord::default(),
        tool_recordings: vec![],
        hitl_questions: vec![],
        persona_runtime: vec![],
        metadata: BTreeMap::new(),
        persisted_path: None,
    };
    let fixture = replay_fixture_from_run(&run);
    let report = evaluate_run_against_fixture(&run, &fixture);
    assert!(report.pass);
    assert!(report.failures.is_empty());
}

#[test]
fn replay_eval_suite_reports_failed_case() {
    let good = RunRecord {
        id: "run_good".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let bad = RunRecord {
        id: "run_bad".to_string(),
        workflow_id: "wf".to_string(),
        status: "failed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let suite = evaluate_run_suite(vec![
        (
            good.clone(),
            replay_fixture_from_run(&good),
            Some("good.json".to_string()),
        ),
        (
            bad,
            replay_fixture_from_run(&good),
            Some("bad.json".to_string()),
        ),
    ]);
    assert!(!suite.pass);
    assert_eq!(suite.total, 2);
    assert_eq!(suite.failed, 1);
    assert!(suite.cases.iter().any(|case| !case.pass));
}

#[test]
fn clarifying_question_eval_requires_matching_hitl_prompt() {
    let run = RunRecord {
        id: "run_clarify".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        hitl_questions: vec![RunHitlQuestionRecord {
            request_id: "hitl_question_1".to_string(),
            prompt: "Which repository should I patch?".to_string(),
            agent: "planner".to_string(),
            trace_id: Some("trace-1".to_string()),
            asked_at: "2026-04-23T12:00:00Z".to_string(),
        }],
        ..Default::default()
    };
    let fixture = ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: "fixture_clarify".to_string(),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        created_at: "2026-04-23T12:00:01Z".to_string(),
        eval_kind: Some("clarifying_question".to_string()),
        clarifying_question: Some(ClarifyingQuestionEvalSpec {
            required_terms: vec!["repository".to_string()],
            forbidden_terms: vec!["branch".to_string()],
            ..Default::default()
        }),
        expected_status: "completed".to_string(),
        stage_assertions: vec![],
        ..Default::default()
    };

    let report = evaluate_run_against_fixture(&run, &fixture);

    assert!(report.pass, "failures: {:?}", report.failures);
}

#[test]
fn eval_suite_manifest_can_fail_on_baseline_diff() {
    let temp_dir = std::env::temp_dir().join(format!("harn-eval-suite-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let baseline_path = temp_dir.join("baseline.json");
    let candidate_path = temp_dir.join("candidate.json");

    let baseline = RunRecord {
        id: "baseline".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let candidate = RunRecord {
        id: "candidate".to_string(),
        workflow_id: "wf".to_string(),
        status: "failed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };

    save_run_record(&baseline, Some(baseline_path.to_str().unwrap())).unwrap();
    save_run_record(&candidate, Some(candidate_path.to_str().unwrap())).unwrap();

    let manifest = EvalSuiteManifest {
        base_dir: Some(temp_dir.display().to_string()),
        cases: vec![EvalSuiteCase {
            label: Some("candidate".to_string()),
            run_path: "candidate.json".to_string(),
            fixture_path: None,
            compare_to: Some("baseline.json".to_string()),
        }],
        ..Default::default()
    };
    let suite = evaluate_run_suite_manifest(&manifest).unwrap();
    assert!(!suite.pass);
    assert_eq!(suite.failed, 1);
    assert!(suite.cases[0].comparison.is_some());
    assert!(suite.cases[0]
        .failures
        .iter()
        .any(|failure| failure.contains("baseline")));
}
