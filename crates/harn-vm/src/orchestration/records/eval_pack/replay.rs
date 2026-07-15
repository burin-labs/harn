use super::*;
use crate::orchestration::ContextPackSuggestion;

pub(super) fn load_eval_pack_case_run(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Result<RunRecord, VmError> {
    if let Some(run_ref) = case.run.as_deref().or(case.run_path.as_deref()) {
        if let Some(fixture) = fixtures_by_id.get(run_ref) {
            return load_run_record_from_fixture_ref(fixture, fixture_base_dir);
        }
        return load_run_record(&resolve_manifest_path(base_dir, run_ref));
    }
    Err(VmError::Runtime(
        "eval pack case is missing run or run_path".to_string(),
    ))
}

pub(super) fn load_eval_pack_case_fixture(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
    run: &RunRecord,
) -> Result<ReplayFixture, VmError> {
    if let Some(fixture_ref) = case.fixture.as_deref().or(case.fixture_path.as_deref()) {
        if let Some(fixture) = fixtures_by_id.get(fixture_ref) {
            return load_replay_fixture_from_ref(fixture, fixture_base_dir);
        }
        return load_replay_fixture(&resolve_manifest_path(base_dir, fixture_ref));
    }
    Ok(run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| replay_fixture_from_run(run)))
}

pub(super) fn eval_pack_case_source_path(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Option<String> {
    let run_ref = case.run.as_deref().or(case.run_path.as_deref())?;
    if let Some(fixture) = fixtures_by_id.get(run_ref) {
        return fixture.path.as_ref().map(|path| {
            resolve_manifest_path(fixture_base_dir, path)
                .display()
                .to_string()
        });
    }
    Some(
        resolve_manifest_path(base_dir, run_ref)
            .display()
            .to_string(),
    )
}

pub(super) fn load_eval_pack_case_friction_events(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Result<Vec<FrictionEvent>, VmError> {
    let event_ref = case.friction_events.as_deref().ok_or_else(|| {
        VmError::Runtime("eval pack friction case is missing friction_events".to_string())
    })?;
    if let Some(fixture) = fixtures_by_id.get(event_ref) {
        return load_friction_events_from_fixture_ref(fixture, fixture_base_dir);
    }
    load_friction_events_from_path(&resolve_manifest_path(base_dir, event_ref))
}

fn load_friction_events_from_fixture_ref(
    fixture: &EvalPackFixtureRef,
    base_dir: Option<&Path>,
) -> Result<Vec<FrictionEvent>, VmError> {
    if let Some(inline) = &fixture.inline {
        return normalize_friction_events_json(inline.clone());
    }
    let path = fixture.path.as_deref().ok_or_else(|| {
        VmError::Runtime(format!(
            "fixture '{}' is missing path or inline friction events",
            fixture.id
        ))
    })?;
    load_friction_events_from_path(&resolve_manifest_path(base_dir, path))
}

fn load_friction_events_from_path(path: &Path) -> Result<Vec<FrictionEvent>, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read friction events fixture: {e}")))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse friction events fixture: {e}")))?;
    normalize_friction_events_json(value)
}

pub(super) fn eval_pack_case_friction_source_path(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Option<String> {
    let event_ref = case.friction_events.as_deref()?;
    if let Some(fixture) = fixtures_by_id.get(event_ref) {
        return fixture.path.as_ref().map(|path| {
            resolve_manifest_path(fixture_base_dir, path)
                .display()
                .to_string()
        });
    }
    Some(
        resolve_manifest_path(base_dir, event_ref)
            .display()
            .to_string(),
    )
}

pub(super) fn friction_suggestion_options(
    case: &EvalPackCase,
    manifest: &EvalPackManifest,
) -> ContextPackSuggestionOptions {
    let min_occurrences = case
        .metadata
        .get("min_occurrences")
        .or_else(|| manifest.metadata.get("min_occurrences"))
        .and_then(|value| value.as_u64())
        .unwrap_or(2) as usize;
    let owner = case
        .metadata
        .get("owner")
        .or_else(|| manifest.metadata.get("owner"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            manifest
                .package
                .as_ref()
                .and_then(|package| package.name.clone())
        });
    ContextPackSuggestionOptions {
        min_occurrences,
        owner,
    }
}

pub(super) fn apply_eval_pack_thresholds(
    run: &RunRecord,
    thresholds: &super::super::types::EvalPackThresholds,
    failures: &mut Vec<String>,
) {
    if let Some(max_stage_count) = thresholds.max_stage_count {
        if run.stages.len() > max_stage_count {
            failures.push(format!(
                "stage count {} exceeds threshold {}",
                run.stages.len(),
                max_stage_count
            ));
        }
    }
    if let Some(max_latency_ms) = thresholds.max_latency_ms {
        let actual = run
            .usage
            .as_ref()
            .map(|usage| usage.total_duration_ms)
            .unwrap_or_default();
        if actual > max_latency_ms {
            failures.push(format!(
                "latency {actual}ms exceeds threshold {max_latency_ms}ms"
            ));
        }
    }
    if let Some(max_cost_usd) = thresholds.max_cost_usd {
        let actual = run
            .usage
            .as_ref()
            .map(|usage| usage.total_cost)
            .unwrap_or_default();
        if actual > max_cost_usd {
            failures.push(format!(
                "cost ${actual:.6} exceeds threshold ${max_cost_usd:.6}"
            ));
        }
    }
    if let Some(max_tokens) = thresholds.max_tokens {
        let actual = run
            .usage
            .as_ref()
            .map(|usage| usage.input_tokens + usage.output_tokens)
            .unwrap_or_default();
        if actual > max_tokens {
            failures.push(format!(
                "token count {actual} exceeds threshold {max_tokens}"
            ));
        }
    }
}

pub(super) fn apply_eval_pack_rubric(
    rubric: &EvalPackRubric,
    run: &RunRecord,
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    match rubric.kind.as_str() {
        "" | "deterministic" | "replay" | "budget" | "hitl" | "side-effect" => {
            apply_eval_pack_thresholds(run, &rubric.thresholds, failures);
            for assertion in &rubric.assertions {
                apply_eval_pack_assertion(rubric, assertion, run, failures);
            }
        }
        "llm-judge" | "llm_as_judge" | "judge" => {
            let severity = normalize_eval_pack_severity(
                rubric.thresholds.severity.as_deref().unwrap_or("blocking"),
            );
            let message = format!(
                "rubric '{}' requires an external LLM judge and was not run locally",
                rubric.id
            );
            if severity == "blocking" {
                failures.push(message);
            } else {
                warnings.push(message);
            }
        }
        other => warnings.push(format!(
            "rubric '{}' has unknown kind '{}' and was not run locally",
            rubric.id, other
        )),
    }
}

pub(super) fn apply_eval_pack_friction_rubric(
    rubric: &EvalPackRubric,
    suggestions: &[ContextPackSuggestion],
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    match rubric.kind.as_str() {
        "" | "deterministic" | "friction" | "context-pack-suggestion" => {
            let mut expectations = Vec::new();
            for assertion in &rubric.assertions {
                match assertion.kind.as_str() {
                    "context-pack-suggestion" | "context_pack_suggestion" | "suggestion" => {
                        let expectation = context_pack_expectation_from_assertion(assertion);
                        expectations.push(expectation);
                    }
                    other => failures.push(format!(
                        "rubric '{}' has unsupported friction assertion kind '{}'",
                        rubric.id, other
                    )),
                }
            }
            failures.extend(evaluate_context_pack_suggestion_expectations(
                suggestions,
                &expectations,
            ));
        }
        other => warnings.push(format!(
            "rubric '{}' has unknown friction kind '{}' and was not run locally",
            rubric.id, other
        )),
    }
}

fn context_pack_expectation_from_assertion(
    assertion: &EvalPackAssertion,
) -> ContextPackSuggestionExpectation {
    let expected = assertion
        .expected
        .as_ref()
        .and_then(|value| value.as_object());
    let expected_string = assertion.expected.as_ref().and_then(|value| value.as_str());
    ContextPackSuggestionExpectation {
        min_suggestions: expected
            .and_then(|map| map.get("min_suggestions"))
            .and_then(|value| value.as_u64())
            .map(|value| value as usize),
        recommended_artifact: expected
            .and_then(|map| map.get("recommended_artifact"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| expected_string.map(str::to_string)),
        title_contains: assertion.contains.clone().or_else(|| {
            expected
                .and_then(|map| map.get("title_contains"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        }),
        manifest_name_contains: expected
            .and_then(|map| map.get("manifest_name_contains"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        required_capability: expected
            .and_then(|map| map.get("required_capability"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        required_output_slot: expected
            .and_then(|map| map.get("required_output_slot"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
    }
}

fn apply_eval_pack_assertion(
    rubric: &EvalPackRubric,
    assertion: &EvalPackAssertion,
    run: &RunRecord,
    failures: &mut Vec<String>,
) {
    match assertion.kind.as_str() {
        "run-status" | "run_status" | "status" => {
            let expected = assertion.expected.as_ref().and_then(|value| value.as_str());
            if let Some(expected) = expected {
                if run.status != expected {
                    failures.push(format!(
                        "rubric '{}' expected run status {}, got {}",
                        rubric.id, expected, run.status
                    ));
                }
            }
        }
        "stage-status" | "stage_status" => {
            let Some(stage_id) = assertion.stage.as_deref() else {
                failures.push(format!(
                    "rubric '{}' stage-status assertion missing stage",
                    rubric.id
                ));
                return;
            };
            let expected = assertion.expected.as_ref().and_then(|value| value.as_str());
            let Some(expected) = expected else {
                failures.push(format!(
                    "rubric '{}' stage-status assertion missing expected string",
                    rubric.id
                ));
                return;
            };
            match run.stages.iter().find(|stage| stage.node_id == stage_id) {
                Some(stage) if stage.status == expected => {}
                Some(stage) => failures.push(format!(
                    "rubric '{}' expected stage {} status {}, got {}",
                    rubric.id, stage_id, expected, stage.status
                )),
                None => failures.push(format!(
                    "rubric '{}' expected stage {} to exist",
                    rubric.id, stage_id
                )),
            }
        }
        "visible-text-contains" | "visible_text_contains" => {
            let Some(needle) = assertion.contains.as_deref() else {
                failures.push(format!(
                    "rubric '{}' visible-text assertion missing contains",
                    rubric.id
                ));
                return;
            };
            let matched = match assertion.stage.as_deref() {
                Some(stage_id) => run
                    .stages
                    .iter()
                    .find(|stage| stage.node_id == stage_id)
                    .and_then(|stage| stage.visible_text.as_deref())
                    .is_some_and(|text| text.contains(needle)),
                None => run
                    .stages
                    .iter()
                    .filter_map(|stage| stage.visible_text.as_deref())
                    .any(|text| text.contains(needle)),
            };
            if !matched {
                failures.push(format!(
                    "rubric '{}' expected visible text to contain {:?}",
                    rubric.id, needle
                ));
            }
        }
        "hitl-question-contains" | "hitl_question_contains" => {
            let Some(needle) = assertion.contains.as_deref() else {
                failures.push(format!(
                    "rubric '{}' HITL assertion missing contains",
                    rubric.id
                ));
                return;
            };
            if !run
                .hitl_questions
                .iter()
                .any(|question| question.prompt.contains(needle))
            {
                failures.push(format!(
                    "rubric '{}' expected HITL question to contain {:?}",
                    rubric.id, needle
                ));
            }
        }
        "" => {}
        other => failures.push(format!(
            "rubric '{}' has unsupported assertion kind '{}'",
            rubric.id, other
        )),
    }
}

pub fn replay_fixture_from_run(run: &RunRecord) -> ReplayFixture {
    ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: new_id("fixture"),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        workflow_name: run.workflow_name.clone(),
        created_at: now_rfc3339(),
        eval_kind: Some("replay".to_string()),
        clarifying_question: None,
        expected_status: run.status.clone(),
        stage_assertions: run
            .stages
            .iter()
            .map(|stage| ReplayStageAssertion {
                node_id: stage.node_id.clone(),
                expected_status: stage.status.clone(),
                expected_outcome: stage.outcome.clone(),
                expected_branch: stage.branch.clone(),
                required_artifact_kinds: stage
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.kind.clone())
                    .collect(),
                visible_text_contains: stage
                    .visible_text
                    .as_ref()
                    .filter(|text| !text.is_empty())
                    .map(|text| text.chars().take(80).collect()),
            })
            .collect(),
    }
}

pub fn evaluate_run_against_fixture(run: &RunRecord, fixture: &ReplayFixture) -> ReplayEvalReport {
    if fixture.eval_kind.as_deref() == Some("clarifying_question") {
        return evaluate_clarifying_question(run, fixture);
    }
    let mut failures = Vec::new();
    if run.status != fixture.expected_status {
        failures.push(format!(
            "run status mismatch: expected {}, got {}",
            fixture.expected_status, run.status
        ));
    }
    let stages_by_id: BTreeMap<&str, &RunStageRecord> =
        run.stages.iter().map(|s| (s.node_id.as_str(), s)).collect();
    for assertion in &fixture.stage_assertions {
        let Some(stage) = stages_by_id.get(assertion.node_id.as_str()) else {
            failures.push(format!("missing stage {}", assertion.node_id));
            continue;
        };
        if stage.status != assertion.expected_status {
            failures.push(format!(
                "stage {} status mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_status, stage.status
            ));
        }
        if stage.outcome != assertion.expected_outcome {
            failures.push(format!(
                "stage {} outcome mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_outcome, stage.outcome
            ));
        }
        if stage.branch != assertion.expected_branch {
            failures.push(format!(
                "stage {} branch mismatch: expected {:?}, got {:?}",
                assertion.node_id, assertion.expected_branch, stage.branch
            ));
        }
        for required_kind in &assertion.required_artifact_kinds {
            if !stage
                .artifacts
                .iter()
                .any(|artifact| &artifact.kind == required_kind)
            {
                failures.push(format!(
                    "stage {} missing artifact kind {}",
                    assertion.node_id, required_kind
                ));
            }
        }
        if let Some(snippet) = &assertion.visible_text_contains {
            let actual = stage.visible_text.clone().unwrap_or_default();
            if !actual.contains(snippet) {
                failures.push(format!(
                    "stage {} visible text does not contain expected snippet {:?}",
                    assertion.node_id, snippet
                ));
            }
        }
    }

    ReplayEvalReport {
        pass: failures.is_empty(),
        failures,
        stage_count: run.stages.len(),
    }
}

fn evaluate_clarifying_question(run: &RunRecord, fixture: &ReplayFixture) -> ReplayEvalReport {
    let mut failures = Vec::new();
    let spec = fixture.clarifying_question.clone().unwrap_or_default();
    let min_questions = clarifying_min_questions(&spec);
    let max_questions = clarifying_max_questions(&spec);
    let questions = &run.hitl_questions;

    if run.status != fixture.expected_status {
        failures.push(format!(
            "run status mismatch: expected {}, got {}",
            fixture.expected_status, run.status
        ));
    }
    if questions.len() < min_questions {
        failures.push(format!(
            "expected at least {min_questions} clarifying question(s), got {}",
            questions.len()
        ));
    }
    if questions.len() > max_questions {
        failures.push(format!(
            "expected at most {max_questions} clarifying question(s), got {}",
            questions.len()
        ));
    }

    let normalized_expected = spec
        .expected_question
        .as_deref()
        .map(normalize_question_text);
    let normalized_accepted = spec
        .accepted_questions
        .iter()
        .map(|question| normalize_question_text(question))
        .collect::<Vec<_>>();
    let required_terms = spec
        .required_terms
        .iter()
        .map(|term| normalize_question_text(term))
        .collect::<Vec<_>>();
    let forbidden_terms = spec
        .forbidden_terms
        .iter()
        .map(|term| normalize_question_text(term))
        .collect::<Vec<_>>();

    let matched = questions.iter().any(|question| {
        let normalized = normalize_question_text(&question.prompt);
        let matches_expected = normalized_expected
            .as_ref()
            .is_none_or(|expected| &normalized == expected)
            && (normalized_accepted.is_empty()
                || normalized_accepted
                    .iter()
                    .any(|candidate| candidate == &normalized));
        let has_required_terms = required_terms
            .iter()
            .all(|term| normalized.contains(term.as_str()));
        let avoids_forbidden_terms = forbidden_terms
            .iter()
            .all(|term| !normalized.contains(term.as_str()));
        matches_expected && has_required_terms && avoids_forbidden_terms
    });

    if !questions.is_empty()
        && (!normalized_accepted.is_empty()
            || normalized_expected.is_some()
            || !required_terms.is_empty()
            || !forbidden_terms.is_empty())
        && !matched
    {
        failures.push(format!(
            "no clarifying question matched fixture; actual questions: {}",
            questions
                .iter()
                .map(|question| format!("{:?}", question.prompt))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    ReplayEvalReport {
        pass: failures.is_empty(),
        failures,
        stage_count: run.stages.len(),
    }
}

pub fn evaluate_run_suite(
    cases: Vec<(RunRecord, ReplayFixture, Option<String>)>,
) -> ReplayEvalSuiteReport {
    let mut reports = Vec::new();
    for (run, fixture, source_path) in cases {
        let report = evaluate_run_against_fixture(&run, &fixture);
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: None,
            pass: report.pass,
            failures: report.failures,
            stage_count: report.stage_count,
            source_path,
            comparison: None,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    }
}
