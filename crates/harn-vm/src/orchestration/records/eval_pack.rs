//! Eval-suite manifest + eval-pack manifest loading, evaluation, replay-fixture comparison.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::{
    evaluate_context_pack_suggestion_expectations, generate_context_pack_suggestions, new_id,
    normalize_friction_events_json, now_rfc3339, parse_json_value, run_persona_eval_ladder,
    ContextPackSuggestionExpectation, ContextPackSuggestionOptions, FrictionEvent,
};
use super::diff::diff_run_records;
use super::json::{clarifying_max_questions, clarifying_min_questions, normalize_question_text};
use super::persistence::load_run_record;
use super::types::{
    EvalPackAssertion, EvalPackCase, EvalPackCaseReport, EvalPackFixtureRef, EvalPackManifest,
    EvalPackReport, EvalPackRubric, EvalSuiteManifest, ReplayEvalCaseReport, ReplayEvalReport,
    ReplayEvalSuiteReport, ReplayFixture, ReplayStageAssertion, RunRecord, RunStageRecord,
};
use crate::value::{VmError, VmValue};

pub fn normalize_eval_suite_manifest(value: &VmValue) -> Result<EvalSuiteManifest, VmError> {
    let mut manifest: EvalSuiteManifest = parse_json_value(value)?;
    if manifest.type_name.is_empty() {
        manifest.type_name = "eval_suite_manifest".to_string();
    }
    if manifest.id.is_empty() {
        manifest.id = new_id("eval_suite");
    }
    Ok(manifest)
}

pub fn load_eval_suite_manifest(path: &Path) -> Result<EvalSuiteManifest, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read eval suite manifest: {e}")))?;
    let mut manifest: EvalSuiteManifest = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse eval suite manifest: {e}")))?;
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn load_eval_pack_manifest(path: &Path) -> Result<EvalPackManifest, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read eval pack manifest: {e}")))?;
    let mut manifest: EvalPackManifest =
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            serde_json::from_str(&content)
                .map_err(|e| VmError::Runtime(format!("failed to parse eval pack JSON: {e}")))?
        } else {
            toml::from_str(&content)
                .map_err(|e| VmError::Runtime(format!("failed to parse eval pack TOML: {e}")))?
        };
    normalize_eval_pack_manifest(&mut manifest);
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn normalize_eval_pack_manifest_value(value: &VmValue) -> Result<EvalPackManifest, VmError> {
    let mut manifest: EvalPackManifest = parse_json_value(value)?;
    normalize_eval_pack_manifest(&mut manifest);
    Ok(manifest)
}

fn normalize_eval_pack_manifest(manifest: &mut EvalPackManifest) {
    if manifest.version == 0 {
        manifest.version = 1;
    }
    if manifest.id.is_empty() {
        manifest.id = manifest
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| new_id("eval_pack"));
    }
    for ladder in &mut manifest.ladders {
        super::super::normalize_persona_eval_ladder_manifest(ladder);
    }
}

fn load_replay_fixture(path: &Path) -> Result<ReplayFixture, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read replay fixture: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse replay fixture: {e}")))
}

fn load_run_record_from_fixture_ref(
    fixture: &EvalPackFixtureRef,
    base_dir: Option<&Path>,
) -> Result<RunRecord, VmError> {
    if let Some(inline) = &fixture.inline {
        let run: RunRecord = serde_json::from_value(inline.clone())
            .map_err(|e| VmError::Runtime(format!("failed to parse inline run record: {e}")))?;
        return Ok(run);
    }
    let path = fixture.path.as_deref().ok_or_else(|| {
        VmError::Runtime(format!(
            "fixture '{}' is missing path or inline run",
            fixture.id
        ))
    })?;
    load_run_record(&resolve_manifest_path(base_dir, path))
}

fn load_replay_fixture_from_ref(
    fixture: &EvalPackFixtureRef,
    base_dir: Option<&Path>,
) -> Result<ReplayFixture, VmError> {
    if let Some(inline) = &fixture.inline {
        return serde_json::from_value(inline.clone())
            .map_err(|e| VmError::Runtime(format!("failed to parse inline replay fixture: {e}")));
    }
    let path = fixture.path.as_deref().ok_or_else(|| {
        VmError::Runtime(format!(
            "fixture '{}' is missing path or inline replay fixture",
            fixture.id
        ))
    })?;
    load_replay_fixture(&resolve_manifest_path(base_dir, path))
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        path_buf
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path_buf)
    } else {
        path_buf
    }
}

pub fn evaluate_run_suite_manifest(
    manifest: &EvalSuiteManifest,
) -> Result<ReplayEvalSuiteReport, VmError> {
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let mut reports = Vec::new();
    for case in &manifest.cases {
        let run_path = resolve_manifest_path(base_dir, &case.run_path);
        let run = load_run_record(&run_path)?;
        let fixture = match &case.fixture_path {
            Some(path) => load_replay_fixture(&resolve_manifest_path(base_dir, path))?,
            None => run
                .replay_fixture
                .clone()
                .unwrap_or_else(|| replay_fixture_from_run(&run)),
        };
        let eval = evaluate_run_against_fixture(&run, &fixture);
        let mut pass = eval.pass;
        let mut failures = eval.failures;
        let comparison = match &case.compare_to {
            Some(path) => {
                let baseline_path = resolve_manifest_path(base_dir, path);
                let baseline = load_run_record(&baseline_path)?;
                let diff = diff_run_records(&baseline, &run);
                if !diff.identical {
                    pass = false;
                    failures.push(format!(
                        "run differs from baseline {} with {} stage changes",
                        baseline_path.display(),
                        diff.stage_diffs.len()
                    ));
                }
                Some(diff)
            }
            None => None,
        };
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: case.label.clone(),
            pass,
            failures,
            stage_count: eval.stage_count,
            source_path: Some(run_path.display().to_string()),
            comparison,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    Ok(ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    })
}

pub fn evaluate_eval_pack_manifest(manifest: &EvalPackManifest) -> Result<EvalPackReport, VmError> {
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let fixture_base_dir_buf = manifest
        .defaults
        .fixture_root
        .as_deref()
        .map(|root| resolve_manifest_path(base_dir, root));
    let fixture_base_dir = fixture_base_dir_buf.as_deref().or(base_dir);
    let fixtures_by_id: BTreeMap<&str, &EvalPackFixtureRef> = manifest
        .fixtures
        .iter()
        .filter(|fixture| !fixture.id.is_empty())
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect();
    let rubrics_by_id: BTreeMap<&str, &EvalPackRubric> = manifest
        .rubrics
        .iter()
        .filter(|rubric| !rubric.id.is_empty())
        .map(|rubric| (rubric.id.as_str(), rubric))
        .collect();

    let mut reports = Vec::new();
    for (index, case) in manifest.cases.iter().enumerate() {
        let case_id = case
            .id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| format!("case_{}", index + 1));
        let label = case
            .name
            .clone()
            .or_else(|| case.id.clone())
            .unwrap_or_else(|| case_id.clone());
        let severity = eval_pack_case_severity(manifest, case);
        let blocking = severity == "blocking";
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        let mut informational = Vec::new();

        if case.friction_events.is_some() {
            let report = evaluate_eval_pack_friction_case(
                manifest,
                case,
                &case_id,
                &label,
                &severity,
                blocking,
                base_dir,
                fixture_base_dir,
                &fixtures_by_id,
                &rubrics_by_id,
            )?;
            reports.push(report);
            continue;
        }

        let run = load_eval_pack_case_run(case, base_dir, fixture_base_dir, &fixtures_by_id)?;
        let fixture =
            load_eval_pack_case_fixture(case, base_dir, fixture_base_dir, &fixtures_by_id, &run)?;
        let eval = evaluate_run_against_fixture(&run, &fixture);
        failures.extend(eval.failures);
        apply_eval_pack_thresholds(&run, &manifest.defaults.thresholds, &mut failures);
        apply_eval_pack_thresholds(&run, &case.thresholds, &mut failures);

        let comparison = match case.compare_to.as_ref().or(manifest.baseline.as_ref()) {
            Some(path) => {
                let baseline_path = resolve_manifest_path(base_dir, path);
                let baseline = load_run_record(&baseline_path)?;
                let diff = diff_run_records(&baseline, &run);
                if !diff.identical {
                    failures.push(format!(
                        "run differs from baseline {} with {} stage changes",
                        baseline_path.display(),
                        diff.stage_diffs.len()
                    ));
                }
                Some(diff)
            }
            None => None,
        };

        for rubric_id in &case.rubrics {
            let Some(rubric) = rubrics_by_id.get(rubric_id.as_str()) else {
                failures.push(format!("case references unknown rubric '{rubric_id}'"));
                continue;
            };
            apply_eval_pack_rubric(rubric, &run, &mut failures, &mut warnings);
        }

        let pass = failures.is_empty() || !blocking;
        if !failures.is_empty() && !blocking {
            if severity == "warning" {
                warnings.append(&mut failures);
            } else {
                informational.append(&mut failures);
            }
        }
        reports.push(EvalPackCaseReport {
            id: case_id,
            label,
            severity,
            pass,
            blocking,
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            source_path: eval_pack_case_source_path(
                case,
                base_dir,
                fixture_base_dir,
                &fixtures_by_id,
            ),
            stage_count: eval.stage_count,
            failures,
            warnings,
            informational,
            comparison,
        });
    }

    let mut ladder_reports = Vec::new();
    for ladder in &manifest.ladders {
        let mut ladder = ladder.clone();
        if ladder.base_dir.is_none() {
            ladder.base_dir = manifest.base_dir.clone();
        }
        ladder_reports.push(run_persona_eval_ladder(&ladder)?);
    }

    let case_total = reports.len();
    let ladder_total = ladder_reports.len();
    let total = case_total + ladder_total;
    let case_blocking_failed = reports
        .iter()
        .filter(|report| report.blocking && !report.failures.is_empty())
        .count();
    let ladder_blocking_failed = ladder_reports
        .iter()
        .filter(|report| report.blocking && !report.pass)
        .count();
    let blocking_failed = case_blocking_failed + ladder_blocking_failed;
    let warning_failed = reports
        .iter()
        .filter(|report| !report.warnings.is_empty())
        .count()
        + ladder_reports
            .iter()
            .filter(|report| !report.pass && report.severity == "warning")
            .count();
    let informational_failed = reports
        .iter()
        .filter(|report| !report.informational.is_empty())
        .count()
        + ladder_reports
            .iter()
            .filter(|report| !report.pass && report.severity == "informational")
            .count();
    let passed = reports.iter().filter(|report| report.pass).count()
        + ladder_reports.iter().filter(|report| report.pass).count();
    Ok(EvalPackReport {
        pack_id: manifest.id.clone(),
        pass: blocking_failed == 0,
        total,
        passed,
        failed: total.saturating_sub(passed),
        blocking_failed,
        warning_failed,
        informational_failed,
        cases: reports,
        ladders: ladder_reports,
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_eval_pack_friction_case(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    case_id: &str,
    label: &str,
    severity: &str,
    blocking: bool,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
    rubrics_by_id: &BTreeMap<&str, &EvalPackRubric>,
) -> Result<EvalPackCaseReport, VmError> {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut informational = Vec::new();
    let events =
        load_eval_pack_case_friction_events(case, base_dir, fixture_base_dir, fixtures_by_id)?;
    let options = friction_suggestion_options(case, manifest);
    let suggestions = generate_context_pack_suggestions(&events, &options);

    for rubric_id in &case.rubrics {
        let Some(rubric) = rubrics_by_id.get(rubric_id.as_str()) else {
            failures.push(format!("case references unknown rubric '{rubric_id}'"));
            continue;
        };
        apply_eval_pack_friction_rubric(rubric, &suggestions, &mut failures, &mut warnings);
    }

    if case.rubrics.is_empty() && suggestions.is_empty() {
        failures.push("friction fixture produced no context-pack suggestions".to_string());
    }

    let pass = failures.is_empty() || !blocking;
    if !failures.is_empty() && !blocking {
        if severity == "warning" {
            warnings.append(&mut failures);
        } else {
            informational.append(&mut failures);
        }
    }

    Ok(EvalPackCaseReport {
        id: case_id.to_string(),
        label: label.to_string(),
        severity: severity.to_string(),
        pass,
        blocking,
        run_id: "friction_events".to_string(),
        workflow_id: String::new(),
        source_path: eval_pack_case_friction_source_path(
            case,
            base_dir,
            fixture_base_dir,
            fixtures_by_id,
        ),
        stage_count: events.len(),
        failures,
        warnings,
        informational,
        comparison: None,
    })
}

fn eval_pack_case_severity(manifest: &EvalPackManifest, case: &EvalPackCase) -> String {
    normalize_eval_pack_severity(
        case.severity
            .as_deref()
            .or(case.thresholds.severity.as_deref())
            .or(manifest.defaults.severity.as_deref())
            .or(manifest.defaults.thresholds.severity.as_deref())
            .unwrap_or("blocking"),
    )
}

fn normalize_eval_pack_severity(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "warn" | "warning" => "warning".to_string(),
        "info" | "informational" => "informational".to_string(),
        _ => "blocking".to_string(),
    }
}

fn load_eval_pack_case_run(
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

fn load_eval_pack_case_fixture(
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

fn eval_pack_case_source_path(
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

fn load_eval_pack_case_friction_events(
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

fn eval_pack_case_friction_source_path(
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

fn friction_suggestion_options(
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

fn apply_eval_pack_thresholds(
    run: &RunRecord,
    thresholds: &super::types::EvalPackThresholds,
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

fn apply_eval_pack_rubric(
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

fn apply_eval_pack_friction_rubric(
    rubric: &EvalPackRubric,
    suggestions: &[super::super::ContextPackSuggestion],
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
