//! End-to-end coverage for the agent_loop trajectory tap (harn#2436).
//!
//! These tests stand in for the manual `harn replay --suite
//! merge-captain-fixtures --crystallize-from agent_loop` flow named in
//! the issue's verification block: they build a synthetic merge-captain
//! trajectory, pipe it through [`ingest_agent_loop_trajectory`], and
//! assert the bundle / source-tag / verifier contract the issue spells
//! out.

use std::collections::BTreeMap;

use harn_vm::orchestration::{
    build_crystallization_bundle, ingest_agent_loop_trajectory, validate_crystallization_bundle,
    write_crystallization_bundle, AgentTurnRecord, AgentTurnToolCall, BundleOptions,
    CrystallizationSideEffect, CrystallizeOptions, TrajectoryTap, TRAJECTORY_SOURCE,
};
use serde_json::json;

fn merge_captain_call(
    iteration: usize,
    name: &str,
    args: &[(&str, serde_json::Value)],
) -> AgentTurnToolCall {
    let mut params = BTreeMap::new();
    let mut raw = serde_json::Map::new();
    for (key, value) in args {
        params.insert((*key).to_string(), value.clone());
        raw.insert((*key).to_string(), value.clone());
    }
    AgentTurnToolCall {
        tool_call_id: format!("turn{iteration}_{name}"),
        tool_name: name.to_string(),
        status: "completed".to_string(),
        raw_input: serde_json::Value::Object(raw),
        raw_output: Some(json!({"ok": true, "tool": name})),
        capabilities: vec!["github.read".to_string()],
        side_effects: vec![CrystallizationSideEffect {
            kind: "github_read".to_string(),
            target: format!("merge-captain/{name}"),
            capability: Some("github.read".to_string()),
            ..CrystallizationSideEffect::default()
        }],
        duration_ms: Some(40),
        parameters: params,
    }
}

fn merge_captain_turn(iteration: usize, repo: &str) -> AgentTurnRecord {
    AgentTurnRecord {
        iteration,
        session_id: "merge_captain_session".to_string(),
        success: true,
        tool_calls: vec![
            merge_captain_call(
                iteration,
                "list_open_prs",
                &[("repo", json!(repo)), ("state", json!("open"))],
            ),
            merge_captain_call(
                iteration,
                "check_ci_status",
                &[("repo", json!(repo)), ("ref", json!("HEAD"))],
            ),
            merge_captain_call(
                iteration,
                "label_pr",
                &[("repo", json!(repo)), ("label", json!("ready"))],
            ),
        ],
        provider: Some("openrouter".to_string()),
        model: Some("anthropic/claude-haiku".to_string()),
        input_tokens: 1200,
        output_tokens: 250,
        duration_ms: Some(800),
        assistant_text: Some(format!("processed {repo}")),
        ..AgentTurnRecord::default()
    }
}

#[test]
fn merge_captain_trajectory_yields_deterministic_skill_with_bundle() {
    // Two consecutive successful turns with identical signatures →
    // exactly the case the spec calls "consecutive successful turns
    // grouped by similarity" — and matches the merge-captain workload
    // that recurs across PR fanout in burin-labs/harn.
    let turns = vec![
        merge_captain_turn(1, "burin-labs/harn"),
        merge_captain_turn(2, "burin-labs/harn"),
        merge_captain_turn(3, "burin-labs/harn"),
    ];
    let tap = TrajectoryTap::new("merge_captain_session")
        .with_workflow_id("merge_captain")
        .with_segment_len(2, 12);
    let result = ingest_agent_loop_trajectory(
        &tap,
        &turns,
        CrystallizeOptions {
            min_examples: 1,
            workflow_name: Some("merge_captain_sweep".to_string()),
            package_name: Some("merge-captain-trajectories".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .expect("trajectory ingestion succeeds")
    .expect("trajectory produces at least one segment");

    // Acceptance criterion 1: TrajectoryTap collector produces
    // candidates from grouped successful agent_loop turns. A single
    // segment trace contains four actions per turn (one model_call +
    // three tool_calls), and the candidate spans all of them.
    let candidate = result
        .artifacts
        .report
        .candidates
        .first()
        .expect("at least one accepted candidate");
    assert!(!candidate.steps.is_empty(), "candidate must have steps");
    assert!(
        candidate
            .steps
            .iter()
            .any(|step| step.name == "list_open_prs"),
        "expected the merge_captain tool calls to land in candidate steps"
    );

    // Acceptance criterion 5: receipt shows source=agent_loop_trajectory.
    assert!(result
        .traces
        .iter()
        .all(|trace| trace.source.as_deref() == Some(TRAJECTORY_SOURCE)));
    assert!(result
        .traces
        .iter()
        .all(|trace| trace.metadata.get("source") == Some(&json!(TRAJECTORY_SOURCE))));

    // Acceptance criterion 2: candidates flow through the existing
    // bundle pipeline. A trajectory bundle is shaped identically to any
    // composition / version-bump bundle and validates on disk.
    let bundle = build_crystallization_bundle(
        result.artifacts.clone(),
        &result.traces,
        BundleOptions {
            team: Some("platform".to_string()),
            repo: Some("burin-labs/harn".to_string()),
            ..BundleOptions::default()
        },
    )
    .expect("bundle build succeeds");
    // The trajectory source tag rides on every BundleSourceTrace via
    // the trace's `source` field — that's what downstream cloud
    // importers/receipts read.
    assert!(bundle
        .manifest
        .source_traces
        .iter()
        .all(|src| src.source_url.as_deref() == Some(TRAJECTORY_SOURCE)));

    let dir = tempfile::tempdir().expect("tempdir");
    write_crystallization_bundle(&bundle, dir.path()).expect("write bundle");
    let validation = validate_crystallization_bundle(dir.path()).expect("validate bundle");
    assert!(
        validation.is_ok(),
        "trajectory bundle validation problems: {:?}",
        validation.problems
    );
    // Acceptance criterion 4 / "auto-generated tests pass": the
    // generated eval pack carries the standard crystallization-shadow
    // rubric so a downstream `harn eval` run treats the bundle the
    // same as any other crystallization output.
    assert!(
        result
            .artifacts
            .eval_pack_toml
            .contains("crystallization-shadow"),
        "eval pack must include the shadow rubric"
    );
}

#[test]
fn verifier_rejects_candidate_when_trace_diverges() {
    // Same as the happy-path test, but we mutate one of the source
    // traces after collection so the candidate's expected_output no
    // longer matches the trace's observed_output. The trajectory
    // verifier (acceptance criterion 3) must catch the divergence and
    // move the candidate to rejected.
    let turns = vec![
        merge_captain_turn(1, "burin-labs/harn"),
        merge_captain_turn(2, "burin-labs/harn"),
    ];
    let tap = TrajectoryTap::new("merge_captain_session");
    let mut result = ingest_agent_loop_trajectory(
        &tap,
        &turns,
        CrystallizeOptions {
            min_examples: 1,
            workflow_name: Some("merge_captain_sweep".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .expect("ingest")
    .expect("trace");
    assert!(!result.artifacts.report.candidates.is_empty());

    // Force a deterministic-step divergence on the source trace so
    // the verifier fails and the candidate flips to rejected.
    for trace in &mut result.traces {
        for action in &mut trace.actions {
            if action.kind == "tool_call" {
                action.observed_output = Some(json!({"ok": false, "tampered": true}));
                action.output = Some(json!({"ok": false, "tampered": true}));
            }
        }
    }
    harn_vm::orchestration::apply_trajectory_verifier(&mut result.artifacts, &result.traces);

    assert!(
        result.artifacts.report.candidates.is_empty(),
        "verifier should have rejected every candidate after trace divergence"
    );
    assert!(
        !result.artifacts.report.rejected_candidates.is_empty(),
        "rejected_candidates should now hold the trajectory candidate"
    );
    assert!(result
        .artifacts
        .report
        .rejected_candidates
        .iter()
        .any(|candidate| candidate
            .rejection_reasons
            .iter()
            .any(|reason| reason.contains("trajectory verifier"))));
}

#[test]
fn failed_turn_splits_segment_into_two_candidates() {
    // A failed turn between two successful runs must not appear in any
    // segment; the surrounding successful turns become separate traces.
    let mut turns = vec![
        merge_captain_turn(1, "burin-labs/harn"),
        merge_captain_turn(2, "burin-labs/harn"),
        merge_captain_turn(3, "burin-labs/harn"),
        merge_captain_turn(4, "burin-labs/harn"),
    ];
    turns[1].success = false;
    let tap = TrajectoryTap::new("merge_captain_session");
    let traces = tap.collect(&turns);
    // Turn 1 alone is below min_segment_len, so the only segment that
    // survives is the post-failure run of turns 3 + 4.
    assert_eq!(traces.len(), 1);
    assert_eq!(
        traces[0].actions.len(),
        merge_captain_turn(3, "x").tool_calls.len() * 2 + 2
    );
}
