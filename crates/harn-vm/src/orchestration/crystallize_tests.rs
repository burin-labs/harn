use super::bundle::redact_trace_for_bundle;
use super::*;
use crate::agent_events::ToolCallStatus;
use crate::composition::{
    composition_crystallization_trace, CompositionChildCall, CompositionChildResult,
    CompositionExecutionReport, CompositionRunEnvelope, COMPOSITION_EXECUTION_SCHEMA_VERSION,
};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};
use crate::{run_replay_oracle_trace, ReplayExpectation, ReplayOracleTrace};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn version_trace(id: &str, version: &str, side_target: &str, fuzzy: bool) -> CrystallizationTrace {
    CrystallizationTrace {
        id: id.to_string(),
        actions: vec![
            CrystallizationAction {
                id: format!("{id}-branch"),
                kind: "tool_call".to_string(),
                name: "git.checkout_branch".to_string(),
                parameters: BTreeMap::from([
                    ("repo_path".to_string(), json!(format!("/tmp/{id}"))),
                    (
                        "branch_name".to_string(),
                        json!(format!("release-{version}")),
                    ),
                ]),
                side_effects: vec![CrystallizationSideEffect {
                    kind: "git_ref".to_string(),
                    target: side_target.to_string(),
                    capability: Some("git.write".to_string()),
                    ..CrystallizationSideEffect::default()
                }],
                capabilities: vec!["git.write".to_string()],
                deterministic: Some(true),
                duration_ms: Some(20),
                ..CrystallizationAction::default()
            },
            CrystallizationAction {
                id: format!("{id}-manifest"),
                kind: "file_mutation".to_string(),
                name: "update_manifest_version".to_string(),
                inputs: json!({"version": version, "path": "harn.toml"}),
                parameters: BTreeMap::from([("version".to_string(), json!(version))]),
                side_effects: vec![CrystallizationSideEffect {
                    kind: "file_write".to_string(),
                    target: "harn.toml".to_string(),
                    capability: Some("fs.write".to_string()),
                    ..CrystallizationSideEffect::default()
                }],
                capabilities: vec!["fs.write".to_string()],
                deterministic: Some(true),
                ..CrystallizationAction::default()
            },
            CrystallizationAction {
                id: format!("{id}-release"),
                kind: if fuzzy { "model_call" } else { "tool_call" }.to_string(),
                name: "prepare_release_notes".to_string(),
                inputs: json!({"release_target": "crates.io", "version": version}),
                parameters: BTreeMap::from([
                    ("release_target".to_string(), json!("crates.io")),
                    ("version".to_string(), json!(version)),
                ]),
                fuzzy: Some(fuzzy),
                deterministic: Some(!fuzzy),
                cost: CrystallizationCost {
                    model_calls: i64::from(fuzzy),
                    input_tokens: if fuzzy { 1200 } else { 0 },
                    output_tokens: if fuzzy { 250 } else { 0 },
                    total_cost_usd: if fuzzy { 0.01 } else { 0.0 },
                    wall_ms: 3000,
                    ..CrystallizationCost::default()
                },
                ..CrystallizationAction::default()
            },
        ],
        ..CrystallizationTrace::default()
    }
}

fn composition_trace(id: &str, run_id: &str, path: &str, output: &str) -> CrystallizationTrace {
    let call_id = format!("{run_id}:0");
    let annotations = ToolAnnotations {
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..ToolAnnotations::default()
    };
    let mut run = CompositionRunEnvelope::read_only(
        run_id,
        "harn",
        "sha256:composition-snippet",
        "sha256:composition-manifest",
    );
    run.result = Some(json!({"text": output}));
    run.stdout = Some(String::new());
    let report = CompositionExecutionReport {
        schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
        ok: true,
        run,
        child_calls: vec![CompositionChildCall {
            run_id: run_id.to_string(),
            tool_call_id: call_id.clone(),
            tool_name: "read_file".to_string(),
            operation_index: 0,
            requested_side_effect_level: SideEffectLevel::ReadOnly,
            annotations: Some(annotations),
            raw_input: json!({"path": path}),
            ..CompositionChildCall::default()
        }],
        child_results: vec![CompositionChildResult {
            run_id: run_id.to_string(),
            tool_call_id: call_id,
            tool_name: "read_file".to_string(),
            operation_index: 0,
            status: ToolCallStatus::Completed,
            raw_output: Some(json!({"text": output})),
            duration_ms: Some(2),
            execution_duration_ms: Some(2),
            ..CompositionChildResult::default()
        }],
        summary: "ok".to_string(),
    };
    serde_json::from_value(composition_crystallization_trace(
        &report,
        &json!({
            "id": id,
            "workflow_id": "composition_read_trace",
            "agent_run_id": format!("{run_id}-agent"),
        }),
    ))
    .unwrap()
}

#[test]
fn composition_traces_crystallize_with_child_receipt_shadow_replay() {
    let traces = vec![
        composition_trace("composition_a", "composition-run-a", "README.md", "hello"),
        composition_trace("composition_b", "composition-run-b", "README.md", "hello"),
    ];
    let artifacts = crystallize_traces(
        traces.clone(),
        CrystallizeOptions {
            min_examples: 2,
            workflow_name: Some("composition_read_trace".to_string()),
            package_name: Some("composition-workflows".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .expect("crystallize composition traces");
    let report = &artifacts.report;
    let selected_id = report
        .selected_candidate_id
        .as_deref()
        .expect("composition candidate selected");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.id == selected_id)
        .expect("selected candidate exists");
    assert_eq!(candidate.examples.len(), 2);
    assert!(candidate.examples.iter().all(|example| example
        .action_ids
        .contains(&"composition_child_0".to_string())));
    assert_eq!(candidate.expected_receipts[0]["kind"], "composition_parent");
    assert_eq!(candidate.expected_receipts[1]["kind"], "composition_child");
    assert_eq!(
        candidate.expected_receipts[1]["tool_call_id"],
        "composition-run-a:0"
    );

    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    assert_eq!(bundle.manifest.kind, BundleKind::PlanOnly);
    assert!(bundle
        .manifest
        .source_traces
        .iter()
        .all(|trace| trace.source_url.as_deref() == Some("composition_run")));

    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();
    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(
        validation.problems.is_empty(),
        "validation problems: {:#?}",
        validation.problems
    );
    let (_, shadow) = shadow_replay_bundle(dir.path()).unwrap();
    assert!(shadow.pass, "shadow failures: {:#?}", shadow.failures);
    assert_eq!(shadow.compared_traces, 2);
    assert!(shadow
        .traces
        .iter()
        .all(|trace| trace.compared_receipts >= 2));
}

#[test]
fn composition_replay_receipts_distinguish_parent_and_child_drift() {
    let first = composition_trace("composition_a", "composition-run-a", "README.md", "hello");
    let mut second = first.clone();
    let replay = second.replay_run.as_mut().expect("composition replay run");
    replay.run_id = "composition-run-replay".to_string();
    replay.effect_receipts[1]["output"] = json!({"text": "changed"});
    let oracle = ReplayOracleTrace {
        name: "composition_child_drift".to_string(),
        expect: ReplayExpectation::Match,
        first_run: first.replay_run.unwrap(),
        second_run: replay.clone(),
        ..ReplayOracleTrace::default()
    };
    let report = run_replay_oracle_trace(&oracle).unwrap();
    assert!(!report.passed);
    let divergence = report.divergence.expect("child drift divergence");
    assert!(
        divergence.path.contains("/effect_receipts/1/output"),
        "unexpected divergence path: {}",
        divergence.path
    );
}

#[test]
fn crystallizes_repeated_version_bump_with_parameters() {
    let traces = (0..5)
        .map(|idx| {
            version_trace(
                &format!("trace_{idx}"),
                &format!("0.7.{idx}"),
                "release-branch",
                false,
            )
        })
        .collect::<Vec<_>>();

    let artifacts = crystallize_traces(
        traces,
        CrystallizeOptions {
            workflow_name: Some("version_bump".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .unwrap();

    let candidate = &artifacts.report.candidates[0];
    assert!(candidate.rejection_reasons.is_empty());
    assert!(candidate.shadow.pass);
    assert_eq!(candidate.examples.len(), 5);
    let params = candidate
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(params.contains("version"));
    assert!(params.contains("repo_path"));
    assert!(params.contains("branch_name"));
    assert!(artifacts.harn_code.contains("pipeline version_bump("));
    assert!(artifacts.eval_pack_toml.contains("crystallization-shadow"));
}

#[test]
fn rejects_divergent_side_effects() {
    let traces = vec![
        version_trace("trace_a", "0.7.1", "release-branch", false),
        version_trace("trace_b", "0.7.2", "main", false),
        version_trace("trace_c", "0.7.3", "release-branch", false),
    ];

    let artifacts = crystallize_traces(traces, CrystallizeOptions::default()).unwrap();

    assert!(artifacts.report.candidates.is_empty());
    assert_eq!(artifacts.report.rejected_candidates.len(), 1);
    assert!(artifacts.report.rejected_candidates[0].rejection_reasons[0]
        .contains("divergent side effects"));
}

#[test]
fn preserves_remaining_fuzzy_segment() {
    let traces = (0..3)
        .map(|idx| {
            version_trace(
                &format!("trace_{idx}"),
                &format!("0.8.{idx}"),
                "release-branch",
                true,
            )
        })
        .collect::<Vec<_>>();

    let artifacts = crystallize_traces(traces, CrystallizeOptions::default()).unwrap();
    let candidate = &artifacts.report.candidates[0];

    assert!(candidate
        .steps
        .iter()
        .any(|step| step.segment == SegmentKind::Fuzzy));
    assert!(candidate.savings.remaining_model_calls > 0);
    assert!(artifacts
        .harn_code
        .contains("review_required: fuzzy segment"));
}

#[test]
fn excludes_unresolved_policy_violations_from_mining() {
    let mut traces = version_traces(2);
    let mut excluded = version_trace("trace_policy_blocked", "0.7.99", "release-branch", false);
    excluded
        .metadata
        .insert("unresolved_policy_violation".to_string(), json!(true));
    traces.push(excluded);

    let artifacts = crystallize_traces(
        traces,
        CrystallizeOptions {
            min_examples: 2,
            ..CrystallizeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(artifacts.report.source_trace_count, 2);
    assert_eq!(artifacts.report.excluded_trace_count, 1);
    assert!(artifacts.report.selected_candidate_id.is_some());
}

fn plan_only_trace(id: &str, suffix: &str) -> CrystallizationTrace {
    CrystallizationTrace {
        id: id.to_string(),
        actions: vec![
            CrystallizationAction {
                id: format!("{id}-classify"),
                kind: "tool_call".to_string(),
                name: "classify_issue".to_string(),
                parameters: BTreeMap::from([
                    ("issue_id".to_string(), json!(format!("HAR-{suffix}"))),
                    ("team_key".to_string(), json!("HAR")),
                ]),
                capabilities: vec!["linear.read".to_string()],
                deterministic: Some(true),
                duration_ms: Some(15),
                ..CrystallizationAction::default()
            },
            CrystallizationAction {
                id: format!("{id}-receipt"),
                kind: "receipt_write".to_string(),
                name: "emit_receipt".to_string(),
                inputs: json!({"summary": format!("plan only #{suffix}"), "kind": "plan"}),
                parameters: BTreeMap::from([
                    ("kind".to_string(), json!("plan")),
                    ("summary".to_string(), json!(format!("plan only #{suffix}"))),
                ]),
                side_effects: vec![CrystallizationSideEffect {
                    kind: "receipt_write".to_string(),
                    target: "tenant_event_log".to_string(),
                    capability: Some("receipt.write".to_string()),
                    ..CrystallizationSideEffect::default()
                }],
                capabilities: vec!["receipt.write".to_string()],
                deterministic: Some(true),
                duration_ms: Some(5),
                ..CrystallizationAction::default()
            },
        ],
        ..CrystallizationTrace::default()
    }
}

fn version_traces(count: usize) -> Vec<CrystallizationTrace> {
    (0..count)
        .map(|idx| {
            version_trace(
                &format!("trace_{idx}"),
                &format!("0.7.{idx}"),
                "release-branch",
                false,
            )
        })
        .collect()
}

#[test]
fn build_bundle_assembles_versioned_manifest() {
    let traces = version_traces(5);
    let artifacts = crystallize_traces(
        traces.clone(),
        CrystallizeOptions {
            workflow_name: Some("version_bump".to_string()),
            package_name: Some("release-workflows".to_string()),
            author: Some("ops@example.com".to_string()),
            approver: Some("lead@example.com".to_string()),
            eval_pack_link: Some("eval-pack://release-workflows/v1".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .unwrap();

    let bundle = build_crystallization_bundle(
        artifacts,
        &traces,
        BundleOptions {
            team: Some("platform".to_string()),
            repo: Some("burin-labs/harn".to_string()),
            ..BundleOptions::default()
        },
    )
    .unwrap();

    let manifest = &bundle.manifest;
    assert_eq!(manifest.schema, BUNDLE_SCHEMA);
    assert_eq!(manifest.schema_version, BUNDLE_SCHEMA_VERSION);
    assert_eq!(manifest.kind, BundleKind::Candidate);
    assert!(!manifest.candidate_id.is_empty());
    assert_eq!(manifest.workflow.name, "version_bump");
    assert_eq!(manifest.workflow.package_name, "release-workflows");
    assert_eq!(manifest.workflow.path, BUNDLE_WORKFLOW_FILE);
    assert_eq!(manifest.team.as_deref(), Some("platform"));
    assert_eq!(manifest.repo.as_deref(), Some("burin-labs/harn"));
    assert_eq!(manifest.external_key, "version-bump");
    assert_eq!(manifest.promotion.rollout_policy, "shadow_then_canary");
    assert_eq!(
        manifest.promotion.author.as_deref(),
        Some("ops@example.com")
    );
    assert_eq!(
        manifest.promotion.approver.as_deref(),
        Some("lead@example.com")
    );
    assert_eq!(manifest.promotion.workflow_version, "0.1.0");
    assert!(manifest.deterministic_steps.len() + manifest.fuzzy_steps.len() > 0);
    assert_eq!(manifest.source_traces.len(), traces.len());
    assert_eq!(manifest.fixtures.len(), traces.len());
    assert!(manifest.fixtures.iter().all(|fixture| fixture.redacted));
    assert!(manifest.redaction.applied);
    assert!(manifest.redaction.fixture_count > 0);
    assert!(manifest
        .eval_pack
        .as_ref()
        .is_some_and(|eval| eval.path == BUNDLE_EVAL_PACK_FILE));
    assert!(manifest
        .required_secrets
        .iter()
        .all(|secret| !secret.is_empty()));
}

#[test]
fn write_bundle_round_trips_through_disk() {
    let traces = version_traces(5);
    let artifacts = crystallize_traces(
        traces.clone(),
        CrystallizeOptions {
            workflow_name: Some("version_bump".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .unwrap();
    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let written = write_crystallization_bundle(&bundle, dir.path()).unwrap();
    assert_eq!(written.candidate_id, bundle.manifest.candidate_id);

    // Files exist on disk.
    for relative in [
        BUNDLE_MANIFEST_FILE,
        BUNDLE_REPORT_FILE,
        BUNDLE_WORKFLOW_FILE,
        BUNDLE_EVAL_PACK_FILE,
    ] {
        assert!(dir.path().join(relative).exists(), "missing {relative}");
    }
    let fixtures_dir = dir.path().join(BUNDLE_FIXTURES_DIR);
    assert!(fixtures_dir.is_dir());
    assert_eq!(
        std::fs::read_dir(&fixtures_dir).unwrap().count(),
        traces.len()
    );

    // Manifest round-trips.
    let (loaded_manifest, loaded_traces) = load_crystallization_bundle(dir.path()).unwrap();
    assert_eq!(loaded_manifest, bundle.manifest);
    assert_eq!(loaded_traces.len(), traces.len());

    // Validation passes.
    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(
        validation.problems.is_empty(),
        "unexpected problems: {:?}",
        validation.problems
    );
    assert!(validation.is_ok());
    assert!(validation.workflow_ok && validation.report_ok);
    assert!(validation.fixtures_ok && validation.redaction_ok);

    // Shadow replay matches the persisted shadow report.
    let (replay_manifest, shadow) = shadow_replay_bundle(dir.path()).unwrap();
    assert_eq!(replay_manifest.candidate_id, bundle.manifest.candidate_id);
    assert!(shadow.pass, "shadow should still pass");
    assert_eq!(shadow.compared_traces, traces.len());
}

#[test]
fn validate_rejects_bundle_with_missing_workflow() {
    let traces = version_traces(3);
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();

    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();
    std::fs::remove_file(dir.path().join(BUNDLE_WORKFLOW_FILE)).unwrap();

    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(!validation.is_ok());
    assert!(validation
        .problems
        .iter()
        .any(|problem| problem.contains("missing workflow file")));
}

#[test]
fn validate_rejects_manifest_paths_that_escape_bundle() {
    let traces = version_traces(3);
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let mut bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    bundle.manifest.workflow.path = "../outside.harn".to_string();
    bundle.manifest.eval_pack = Some(BundleEvalPackRef {
        path: "C:\\outside\\harn.eval.toml".to_string(),
        link: None,
    });
    bundle.manifest.fixtures[0].path = "/tmp/outside.json".to_string();

    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();

    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(!validation.is_ok());
    assert!(validation
        .problems
        .iter()
        .any(|problem| problem.contains("workflow path")));
    assert!(validation
        .problems
        .iter()
        .any(|problem| problem.contains("eval_pack path")));
    assert!(validation
        .problems
        .iter()
        .any(|problem| problem.contains("fixture path")));
}

#[test]
fn load_rejects_fixture_paths_that_escape_bundle() {
    let traces = version_traces(3);
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let mut bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    bundle.manifest.fixtures[0].path = "../outside.json".to_string();

    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();

    let error = load_crystallization_bundle(dir.path()).unwrap_err();
    assert!(error
        .to_string()
        .contains("fixture path \"../outside.json\" must stay inside the bundle"));
}

#[test]
fn validate_rejects_bundle_with_unredacted_fixture() {
    let traces = version_traces(3);
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let mut bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    // Force a fixture to claim it is unredacted; this must trip
    // validation so a malicious or careless producer cannot ship raw
    // private payloads under the bundle contract.
    bundle.manifest.fixtures[0].redacted = false;
    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();

    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(!validation.is_ok());
    assert!(validation
        .problems
        .iter()
        .any(|problem| problem.contains("not marked redacted")));
}

#[test]
fn validate_rejects_unsupported_schema_version() {
    let traces = version_traces(3);
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let mut bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    bundle.manifest.schema_version = BUNDLE_SCHEMA_VERSION + 1;
    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();

    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(!validation.is_ok());
    assert!(validation
        .problems
        .iter()
        .any(|problem| problem.contains("schema_version")));
}

#[test]
fn redacts_secret_like_values_in_fixtures() {
    // Build secret-shaped strings at runtime so we exercise the
    // redaction prefixes (`xoxb-`, `ghp_`, `sk-`) without checking in
    // source that looks like a real credential to secret scanners.
    let slack_prefix = format!("{}{}", "xo", "xb-");
    let github_prefix = format!("{}{}", "gh", "p_");
    let openai_prefix = "sk-".to_string();
    let pad = "A".repeat(48);
    let slack_secret = format!("{slack_prefix}1234567890-{pad}");
    let github_secret = format!("{github_prefix}{pad}");
    let openai_secret = format!("{openai_prefix}{pad}");

    let mut secret_action = CrystallizationAction {
        id: "secret".to_string(),
        kind: "tool_call".to_string(),
        name: "post_release_to_slack".to_string(),
        parameters: BTreeMap::from([
            ("slack_token".to_string(), json!(slack_secret)),
            ("channel".to_string(), json!("#releases")),
        ]),
        inputs: json!({
            "authorization": format!("Bearer {github_secret}"),
            "version": "0.7.1",
        }),
        ..CrystallizationAction::default()
    };
    secret_action
        .metadata
        .insert("api_key".to_string(), json!(openai_secret));

    let mut trace = CrystallizationTrace {
        id: "trace_secret".to_string(),
        actions: vec![secret_action],
        ..CrystallizationTrace::default()
    };
    redact_trace_for_bundle(&mut trace);
    let action = &trace.actions[0];
    assert_eq!(
        action.parameters.get("slack_token"),
        Some(&json!("[redacted]"))
    );
    assert_eq!(action.parameters.get("channel"), Some(&json!("#releases")));
    let inputs = action.inputs.as_object().unwrap();
    assert_eq!(inputs.get("authorization"), Some(&json!("[redacted]")));
    assert_eq!(inputs.get("version"), Some(&json!("0.7.1")));
    assert_eq!(action.metadata.get("api_key"), Some(&json!("[redacted]")));
}

#[test]
fn plan_only_fixture_yields_plan_only_kind() {
    let traces = (0..3)
        .map(|idx| plan_only_trace(&format!("plan_{idx}"), &format!("{idx}")))
        .collect::<Vec<_>>();
    let artifacts = crystallize_traces(
        traces.clone(),
        CrystallizeOptions {
            workflow_name: Some("plan_only_triage".to_string()),
            ..CrystallizeOptions::default()
        },
    )
    .unwrap();
    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    assert_eq!(bundle.manifest.kind, BundleKind::PlanOnly);
    assert_eq!(bundle.manifest.risk_level, "low");
}

#[test]
fn rejected_bundle_has_rejected_kind() {
    let traces = vec![
        version_trace("trace_a", "0.7.1", "release-branch", false),
        version_trace("trace_b", "0.7.2", "main", false),
        version_trace("trace_c", "0.7.3", "release-branch", false),
    ];
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    assert_eq!(bundle.manifest.kind, BundleKind::Rejected);
    assert!(bundle.manifest.candidate_id.is_empty());
    assert!(!bundle.manifest.rejection_reasons.is_empty());
    assert!(bundle.fixtures.is_empty());
}

#[test]
fn validate_round_trips_rejected_bundle() {
    let traces = vec![
        version_trace("trace_a", "0.7.1", "release-branch", false),
        version_trace("trace_b", "0.7.2", "main", false),
        version_trace("trace_c", "0.7.3", "release-branch", false),
    ];
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();

    let validation = validate_crystallization_bundle(dir.path()).unwrap();
    assert!(validation.is_ok(), "{:?}", validation.problems);
    assert_eq!(validation.kind, BundleKind::Rejected);
}

#[test]
fn shadow_replay_fails_when_fixture_diverges() {
    let traces = version_traces(3);
    let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
    let bundle =
        build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_crystallization_bundle(&bundle, dir.path()).unwrap();

    // Tamper with one redacted fixture so its action list no longer
    // matches the candidate signature; the replay must fail without
    // panicking.
    let fixture_dir = dir.path().join(BUNDLE_FIXTURES_DIR);
    let some_fixture = std::fs::read_dir(&fixture_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut tampered: CrystallizationTrace =
        serde_json::from_slice(&std::fs::read(&some_fixture).unwrap()).unwrap();
    tampered.actions.truncate(1);
    std::fs::write(&some_fixture, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();

    let (_, shadow) = shadow_replay_bundle(dir.path()).unwrap();
    assert!(!shadow.pass);
    assert!(!shadow.failures.is_empty());
}
