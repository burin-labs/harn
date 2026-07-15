//! Eval-suite manifest + eval-pack manifest loading, evaluation, replay-fixture comparison.

mod ledger;
mod live;
mod replay;
mod report;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::event_log::EventLog;
use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

use super::super::{
    evaluate_context_pack_suggestion_expectations, generate_context_pack_suggestions, new_id,
    normalize_friction_events_json, now_rfc3339, parse_json_value, run_persona_eval_ladder,
    ContextPackSuggestionExpectation, ContextPackSuggestionOptions, FrictionEvent,
};
use super::diff::diff_run_records;
use super::json::{clarifying_max_questions, clarifying_min_questions, normalize_question_text};
use super::persistence::load_run_record;
use super::types::{
    EvalLedgerAppendReport, EvalLedgerFingerprintMismatch, EvalLedgerPriorCommitReport,
    EvalLedgerProvenance, EvalLedgerReadReport, EvalLedgerResumeCell, EvalLedgerResumePlan,
    EvalLedgerRow, EvalPackAssertion, EvalPackCase, EvalPackCaseReport, EvalPackCommandObject,
    EvalPackCommandSpec, EvalPackFixtureRef, EvalPackManifest, EvalPackReliabilityBreakdown,
    EvalPackReliabilityReport, EvalPackReport, EvalPackRubric, EvalPackRunState,
    EvalPackSplitValidationReport, EvalPackStatsReport, EvalPackStatsRow, EvalPackTrialReport,
    EvalSuiteManifest, ReplayEvalCaseReport, ReplayEvalReport, ReplayEvalSuiteReport,
    ReplayFixture, ReplayStageAssertion, RunDiffReport, RunRecord, RunStageRecord,
};
use crate::value::{VmError, VmValue};

use ledger::eval_pack_manifest_model;
pub use ledger::{
    eval_ledger_append_rows_report, eval_ledger_prior_commit_rows_report, eval_ledger_read_report,
    eval_ledger_resume_plan_report,
};
use live::*;
use replay::*;
pub use replay::{evaluate_run_against_fixture, evaluate_run_suite, replay_fixture_from_run};
use report::*;

const EVAL_LEDGER_ROW_SCHEMA: &str = "harn.eval.ledger.row.v1";
const EVAL_LEDGER_RUN_STATE_SCHEMA: &str = "harn.eval.run-state.v1";
const EVAL_LEDGER_RESUME_PLAN_SCHEMA: &str = "harn.eval.resume-plan.v1";
const EVAL_LEDGER_ROW_KIND: &str = "eval.ledger.row";
const EVAL_LEDGER_RUN_STATE_KIND: &str = "eval.ledger.run_state";
const EVAL_LEDGER_TOPIC_PREFIX: &str = "eval.ledger";
const EVAL_LEDGER_IDENTITY_HEADER: &str = "eval_ledger_identity";
const EVAL_LEDGER_QUEUE_DEPTH: usize =
    crate::runtime_limits::RuntimeLimits::DEFAULT.default_event_log_queue_depth;
const EVAL_LEDGER_READ_BATCH_LIMIT: usize = 1024;
const LIVE_EXECUTOR_REQUEST_SCHEMA: &str = "harn.eval.live_verify.executor_request.v1";
const DEFAULT_LIVE_EXECUTOR_TIMEOUT_SECONDS: f64 = 600.0;
const DEFAULT_LIVE_VERIFY_TIMEOUT_SECONDS: f64 = 120.0;

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct EvalLedgerOptions {
    namespace: Option<String>,
    suite: Option<String>,
    model: Option<String>,
    split: Option<String>,
    commit: Option<String>,
    branch: Option<String>,
    #[serde(alias = "case")]
    case_name: Option<String>,
    case_fingerprint: Option<String>,
    harness_config_fingerprint: Option<String>,
    limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvalPackCaseKind {
    Replay,
    Friction,
    LiveVerify,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct EvalPackLiveVerifyOutcome {
    pub verification: Option<String>,
    #[serde(alias = "verificationExitCode")]
    pub verification_exit_code: Option<i64>,
    #[serde(alias = "pass", alias = "success")]
    pub passed: Option<bool>,
    #[serde(alias = "timedOut")]
    pub timed_out: bool,
    #[serde(alias = "wallTimeSeconds")]
    pub wall_time_seconds: f64,
    #[serde(alias = "costUsd")]
    pub cost_usd: f64,
    #[serde(default, alias = "producedPaths")]
    pub produced_paths: Vec<String>,
    #[serde(default, alias = "toolCallSummary", alias = "tool_summary")]
    pub tool_call_summary: serde_json::Value,
    pub failures: Vec<String>,
    pub warnings: Vec<String>,
    pub informational: Vec<String>,
    #[serde(alias = "runId")]
    pub run_id: Option<String>,
    #[serde(alias = "workflowId")]
    pub workflow_id: Option<String>,
    #[serde(alias = "sourcePath")]
    pub source_path: Option<String>,
    #[serde(alias = "stageCount")]
    pub stage_count: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct EvalPackLiveExecutorRequest {
    pub executor: EvalPackCommandSpec,
    pub payload: serde_json::Value,
    pub manifest_id: String,
    pub case: EvalPackCase,
    pub case_id: String,
    pub trial: usize,
    pub trials: usize,
    pub workspace: PathBuf,
    pub base_dir: Option<PathBuf>,
}

pub trait EvalPackLiveExecutor {
    fn execute(
        &mut self,
        request: EvalPackLiveExecutorRequest,
    ) -> Result<EvalPackLiveVerifyOutcome, VmError>;
}

struct EvalPackShellLiveExecutor;

impl EvalPackLiveExecutor for EvalPackShellLiveExecutor {
    fn execute(
        &mut self,
        request: EvalPackLiveExecutorRequest,
    ) -> Result<EvalPackLiveVerifyOutcome, VmError> {
        let output = run_eval_pack_command(
            &request.executor,
            &request.workspace,
            Some(&request.payload),
            DEFAULT_LIVE_EXECUTOR_TIMEOUT_SECONDS,
        )?;
        let mut failures = Vec::new();
        let mut outcome = live_outcome_from_executor_output(output, &mut failures);
        outcome.failures.extend(failures);
        Ok(outcome)
    }
}

#[derive(Clone, Debug)]
struct EvalPackCommandOutput {
    exit_code: i64,
    stdout: String,
    stderr: String,
    timed_out: bool,
    wall_time_seconds: f64,
}

struct EvalPackLedgerRun {
    log: Arc<crate::event_log::AnyEventLog>,
    topic: crate::event_log::Topic,
    rows: Vec<EvalLedgerRow>,
    suite: String,
    model: String,
    commit: String,
    branch: Option<String>,
    provenance: EvalLedgerProvenance,
    inserted: usize,
    duplicates: usize,
    fingerprint_refusals: Vec<EvalLedgerFingerprintMismatch>,
}

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
    normalize_eval_pack_manifest(&mut manifest)?;
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn normalize_eval_pack_manifest_value(value: &VmValue) -> Result<EvalPackManifest, VmError> {
    let mut manifest: EvalPackManifest = parse_json_value(value)?;
    normalize_eval_pack_manifest(&mut manifest)?;
    Ok(manifest)
}

fn normalize_eval_pack_manifest(manifest: &mut EvalPackManifest) -> Result<(), VmError> {
    if manifest.version == 0 {
        manifest.version = 1;
    }
    if manifest.trials == 0 {
        manifest.trials = 1;
    }
    if manifest.id.is_empty() {
        manifest.id = manifest
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| new_id("eval_pack"));
    }
    let rubrics_by_id = manifest
        .rubrics
        .iter()
        .filter(|rubric| !rubric.id.is_empty())
        .map(|rubric| (rubric.id.as_str(), rubric))
        .collect::<BTreeMap<_, _>>();
    let fixtures_by_id = manifest
        .fixtures
        .iter()
        .filter(|fixture| !fixture.id.is_empty())
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect::<BTreeMap<_, _>>();
    for case in &mut manifest.cases {
        if case.trials == Some(0) {
            return Err(VmError::Runtime(format!(
                "eval pack case '{}' has trials = 0",
                case.id.as_deref().unwrap_or("<unnamed>")
            )));
        }
        case.case_fingerprint =
            eval_pack_case_fingerprint_with_refs(case, &rubrics_by_id, &fixtures_by_id)?;
    }
    for ladder in &mut manifest.ladders {
        super::super::normalize_persona_eval_ladder_manifest(ladder);
    }
    Ok(())
}

pub fn eval_pack_case_fingerprint(case: &EvalPackCase) -> Result<String, VmError> {
    eval_pack_case_fingerprint_with_refs(case, &BTreeMap::new(), &BTreeMap::new())
}

fn eval_pack_case_fingerprint_with_refs(
    case: &EvalPackCase,
    rubrics_by_id: &BTreeMap<&str, &EvalPackRubric>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Result<String, VmError> {
    let mut task = BTreeMap::new();
    insert_json_field(&mut task, "kind", &normalized_eval_pack_case_kind(case))?;
    insert_json_field(&mut task, "run", &case.run)?;
    insert_json_field(&mut task, "run_path", &case.run_path)?;
    insert_json_field(&mut task, "friction_events", &case.friction_events)?;
    insert_json_field(&mut task, "task", &case.task)?;
    insert_json_field(&mut task, "workspace", &case.workspace)?;
    insert_json_field(&mut task, "project", &case.project)?;

    let mut expected_outputs = BTreeMap::new();
    insert_json_field(&mut expected_outputs, "fixture", &case.fixture)?;
    insert_json_field(&mut expected_outputs, "fixture_path", &case.fixture_path)?;
    insert_json_field(
        &mut expected_outputs,
        "expected_output_paths",
        &case.expected_output_paths,
    )?;
    insert_json_field(
        &mut expected_outputs,
        "required_output_snippets",
        &case.required_output_snippets,
    )?;
    if let Some(fixture_ref) = case.fixture.as_deref().or(case.fixture_path.as_deref()) {
        if let Some(fixture) = fixtures_by_id.get(fixture_ref) {
            insert_json_field(&mut expected_outputs, "fixture_ref", *fixture)?;
        }
    }

    let resolved_rubrics = case
        .rubrics
        .iter()
        .filter_map(|rubric_id| rubrics_by_id.get(rubric_id.as_str()))
        .map(|rubric| {
            serde_json::to_value(rubric)
                .map_err(|e| VmError::Runtime(format!("failed to encode eval pack rubric: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut verify = BTreeMap::new();
    insert_json_field(&mut verify, "compare_to", &case.compare_to)?;
    insert_json_field(&mut verify, "verify_command", &case.verify_command)?;
    insert_json_field(&mut verify, "tool_budgets", &case.tool_budgets)?;
    insert_json_field(&mut verify, "rubric_ids", &case.rubrics)?;
    verify.insert(
        "rubrics".to_string(),
        serde_json::Value::Array(resolved_rubrics),
    );

    let mut flags = BTreeMap::new();
    insert_json_field(&mut flags, "severity", &case.severity)?;
    insert_json_field(&mut flags, "thresholds", &case.thresholds)?;
    insert_json_field(&mut flags, "metadata", &case.metadata)?;
    insert_json_field(&mut flags, "executor", &case.executor)?;

    let mut payload = BTreeMap::new();
    payload.insert("task".to_string(), encode_json(&task)?);
    payload.insert(
        "expected_outputs".to_string(),
        encode_json(&expected_outputs)?,
    );
    payload.insert("verify".to_string(), encode_json(&verify)?);
    payload.insert("flags".to_string(), encode_json(&flags)?);
    fingerprint_json(&payload)
}

pub fn eval_pack_harness_config_fingerprint(
    manifest: &EvalPackManifest,
) -> Result<String, VmError> {
    let rubric_harness = manifest
        .rubrics
        .iter()
        .map(|rubric| {
            let mut item = BTreeMap::new();
            insert_json_field(&mut item, "id", &rubric.id)?;
            insert_json_field(&mut item, "kind", &rubric.kind)?;
            insert_json_field(&mut item, "prompt", &rubric.prompt)?;
            insert_json_field(&mut item, "judge", &rubric.judge)?;
            encode_json(&item)
        })
        .collect::<Result<Vec<_>, VmError>>()?;
    let mut harness_metadata = BTreeMap::new();
    for key in [
        "model",
        "provider",
        "route",
        "prompt",
        "promptVersion",
        "prompt_version",
        "toolFormat",
        "tool_format",
        "pipelineRev",
        "pipeline_rev",
        "pipelineRevision",
        "pipeline_revision",
        "harnVersion",
        "harn_version",
        "harness",
        "harnessConfig",
        "harness_config",
    ] {
        if let Some(value) = manifest.metadata.get(key) {
            harness_metadata.insert(key.to_string(), value.clone());
        }
    }

    let mut payload = BTreeMap::new();
    insert_json_field(&mut payload, "executor", &manifest.executor)?;
    insert_json_field(&mut payload, "manifest_judge", &manifest.judge)?;
    insert_json_field(&mut payload, "default_judge", &manifest.defaults.judge)?;
    insert_json_field(&mut payload, "package", &manifest.package)?;
    payload.insert(
        "harness_metadata".to_string(),
        encode_json(&harness_metadata)?,
    );
    payload.insert(
        "rubric_harness".to_string(),
        serde_json::Value::Array(rubric_harness),
    );
    fingerprint_json(&payload)
}

fn insert_json_field<T: serde::Serialize>(
    map: &mut BTreeMap<String, serde_json::Value>,
    key: &str,
    value: &T,
) -> Result<(), VmError> {
    map.insert(key.to_string(), encode_json(value)?);
    Ok(())
}

fn encode_json<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, VmError> {
    serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("failed to encode eval pack fingerprint: {e}")))
}

fn fingerprint_json<T: serde::Serialize>(value: &T) -> Result<String, VmError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| VmError::Runtime(format!("failed to encode eval pack fingerprint: {e}")))?;
    let digest = hex::encode(Sha256::digest(bytes));
    Ok(digest.chars().take(16).collect())
}

fn eval_pack_case_kind(case: &EvalPackCase) -> EvalPackCaseKind {
    match normalized_eval_pack_case_kind(case).as_str() {
        "live-verify" => EvalPackCaseKind::LiveVerify,
        "friction" => EvalPackCaseKind::Friction,
        _ => EvalPackCaseKind::Replay,
    }
}

fn normalized_eval_pack_case_kind(case: &EvalPackCase) -> String {
    match case
        .kind
        .as_deref()
        .map(|kind| kind.trim().to_ascii_lowercase().replace('_', "-"))
        .as_deref()
    {
        Some("live") | Some("live-verify") | Some("verify-live") => "live-verify".to_string(),
        Some("friction") | Some("context-pack-friction") => "friction".to_string(),
        Some("replay") | Some("fixture") | Some("run-record") => "replay".to_string(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ if case.task.is_some()
            || case.workspace.is_some()
            || case.project.is_some()
            || case.verify_command.is_some()
            || !case.expected_output_paths.is_empty()
            || !case.required_output_snippets.is_empty() =>
        {
            "live-verify".to_string()
        }
        _ if case.friction_events.is_some() => "friction".to_string(),
        _ => "replay".to_string(),
    }
}

pub fn validate_eval_pack_split(
    manifest: &EvalPackManifest,
) -> Result<EvalPackSplitValidationReport, VmError> {
    let report = eval_pack_split_validation_report(manifest);
    if !report.valid {
        return Err(VmError::Runtime(format!(
            "eval pack split invalid: {}",
            render_split_validation_errors(&report).join("; ")
        )));
    }
    Ok(report)
}

fn eval_pack_split_validation_report(manifest: &EvalPackManifest) -> EvalPackSplitValidationReport {
    let case_ids = eval_pack_case_ids(manifest);
    let mut duplicate_case_ids = duplicates(&case_ids);
    duplicate_case_ids.sort();

    let case_set = case_ids.iter().cloned().collect::<BTreeSet<_>>();
    let Some(split) = &manifest.split else {
        return EvalPackSplitValidationReport {
            valid: duplicate_case_ids.is_empty(),
            case_count: case_ids.len(),
            covered_count: 0,
            duplicate_case_ids,
            ..EvalPackSplitValidationReport::default()
        };
    };

    let mut duplicate_partition_cases = Vec::new();
    let mut unknown_cases = Vec::new();
    let mut seen_by_case: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (partition, cases) in &split.partitions {
        let mut local_seen = BTreeSet::new();
        for case_id in cases {
            if !local_seen.insert(case_id.clone()) {
                duplicate_partition_cases.push(format!("{partition}:{case_id}"));
            }
            if !case_set.contains(case_id) {
                unknown_cases.push(format!("{partition}:{case_id}"));
            }
            let partitions = seen_by_case.entry(case_id.clone()).or_default();
            if !partitions.contains(partition) {
                partitions.push(partition.clone());
            }
        }
    }

    let mut overlap_cases = seen_by_case
        .iter()
        .filter(|(case_id, partitions)| case_set.contains(*case_id) && partitions.len() > 1)
        .map(|(case_id, partitions)| format!("{case_id}:{}", partitions.join(",")))
        .collect::<Vec<_>>();
    let mut missing_cases = case_set
        .iter()
        .filter(|case_id| !seen_by_case.contains_key(*case_id))
        .cloned()
        .collect::<Vec<_>>();
    duplicate_partition_cases.sort();
    unknown_cases.sort();
    overlap_cases.sort();
    missing_cases.sort();

    let covered_count = case_set
        .iter()
        .filter(|case_id| seen_by_case.contains_key(*case_id))
        .count();
    let valid = duplicate_case_ids.is_empty()
        && duplicate_partition_cases.is_empty()
        && unknown_cases.is_empty()
        && overlap_cases.is_empty()
        && missing_cases.is_empty();
    EvalPackSplitValidationReport {
        valid,
        partitions: split.partitions.clone(),
        case_count: case_ids.len(),
        covered_count,
        duplicate_case_ids,
        duplicate_partition_cases,
        overlap_cases,
        unknown_cases,
        missing_cases,
    }
}

fn eval_pack_case_ids(manifest: &EvalPackManifest) -> Vec<String> {
    manifest
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| eval_pack_case_id(case, index))
        .collect()
}

fn eval_pack_case_id(case: &EvalPackCase, index: usize) -> String {
    case.id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("case_{}", index + 1))
}

fn duplicates(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            duplicates.insert(value.clone());
        }
    }
    duplicates.into_iter().collect()
}

fn render_split_validation_errors(report: &EvalPackSplitValidationReport) -> Vec<String> {
    let mut errors = Vec::new();
    if !report.duplicate_case_ids.is_empty() {
        errors.push(format!(
            "duplicate case ids: {}",
            report.duplicate_case_ids.join(", ")
        ));
    }
    if !report.duplicate_partition_cases.is_empty() {
        errors.push(format!(
            "duplicate partition entries: {}",
            report.duplicate_partition_cases.join(", ")
        ));
    }
    if !report.overlap_cases.is_empty() {
        errors.push(format!(
            "overlapping cases: {}",
            report.overlap_cases.join(", ")
        ));
    }
    if !report.unknown_cases.is_empty() {
        errors.push(format!(
            "unknown cases: {}",
            report.unknown_cases.join(", ")
        ));
    }
    if !report.missing_cases.is_empty() {
        errors.push(format!(
            "missing cases: {}",
            report.missing_cases.join(", ")
        ));
    }
    if errors.is_empty() {
        errors.push("unknown split validation error".to_string());
    }
    errors
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
    let mut live_executor = EvalPackShellLiveExecutor;
    evaluate_eval_pack_manifest_inner(manifest, false, None, &mut live_executor)
}

pub fn evaluate_eval_pack_manifest_resumable(
    manifest: &EvalPackManifest,
    ledger_options: Option<serde_json::Value>,
) -> Result<EvalPackReport, VmError> {
    let mut live_executor = EvalPackShellLiveExecutor;
    evaluate_eval_pack_manifest_inner(manifest, true, ledger_options, &mut live_executor)
}

pub fn evaluate_eval_pack_manifest_with_live_executor(
    manifest: &EvalPackManifest,
    live_executor: &mut dyn EvalPackLiveExecutor,
) -> Result<EvalPackReport, VmError> {
    evaluate_eval_pack_manifest_inner(manifest, false, None, live_executor)
}

pub fn evaluate_eval_pack_manifest_resumable_with_live_executor(
    manifest: &EvalPackManifest,
    ledger_options: Option<serde_json::Value>,
    live_executor: &mut dyn EvalPackLiveExecutor,
) -> Result<EvalPackReport, VmError> {
    evaluate_eval_pack_manifest_inner(manifest, true, ledger_options, live_executor)
}

fn evaluate_eval_pack_manifest_inner(
    manifest: &EvalPackManifest,
    ledger_enabled: bool,
    ledger_options: Option<serde_json::Value>,
    live_executor: &mut dyn EvalPackLiveExecutor,
) -> Result<EvalPackReport, VmError> {
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

    let split_report = validate_eval_pack_split(manifest)?;
    let split_by_case = split_by_case_id(&split_report);
    let harness_config_fingerprint = eval_pack_harness_config_fingerprint(manifest)?;
    let mut ledger = if ledger_enabled {
        Some(EvalPackLedgerRun::start(
            manifest,
            base_dir,
            ledger_options,
        )?)
    } else {
        None
    };
    let mut requested_cells = 0usize;
    let mut skipped_cells = 0usize;
    let mut executed_cells = 0usize;
    let mut reports = Vec::new();
    for (index, case) in manifest.cases.iter().enumerate() {
        let case_id = eval_pack_case_id(case, index);
        let label = case
            .name
            .clone()
            .or_else(|| case.id.clone())
            .unwrap_or_else(|| case_id.clone());
        let severity = eval_pack_case_severity(manifest, case);
        let blocking = severity == "blocking";
        let trial_count = case.trials.unwrap_or(manifest.trials);
        let split = split_by_case.get(&case_id).cloned();
        requested_cells += trial_count;
        let mut trials = Vec::with_capacity(trial_count);
        for trial in 1..=trial_count {
            if let Some(ledger) = ledger.as_mut() {
                if let Some(row) = ledger.replay_row_for_cell(
                    &case_id,
                    split.as_deref(),
                    trial,
                    &case.case_fingerprint,
                    &harness_config_fingerprint,
                ) {
                    skipped_cells += 1;
                    trials.push(eval_pack_trial_report_from_ledger_row(&row, blocking));
                    continue;
                }
            }
            let report = match eval_pack_case_kind(case) {
                EvalPackCaseKind::LiveVerify => evaluate_eval_pack_live_verify_trial(
                    manifest,
                    case,
                    &case_id,
                    trial,
                    trial_count,
                    &severity,
                    blocking,
                    base_dir,
                    live_executor,
                )?,
                EvalPackCaseKind::Friction => evaluate_eval_pack_friction_trial(
                    manifest,
                    case,
                    trial,
                    &severity,
                    blocking,
                    base_dir,
                    fixture_base_dir,
                    &fixtures_by_id,
                    &rubrics_by_id,
                )?,
                EvalPackCaseKind::Replay => evaluate_eval_pack_run_trial(
                    manifest,
                    case,
                    trial,
                    &severity,
                    blocking,
                    base_dir,
                    fixture_base_dir,
                    &fixtures_by_id,
                    &rubrics_by_id,
                )?,
            };
            if let Some(ledger) = ledger.as_mut() {
                let row = eval_ledger_row_from_trial(
                    case,
                    &case_id,
                    split.clone(),
                    &ledger.suite,
                    &ledger.model,
                    &ledger.commit,
                    &ledger.provenance,
                    &harness_config_fingerprint,
                    &report,
                );
                ledger.append_trial_row(row)?;
            }
            executed_cells += 1;
            trials.push(report);
        }
        reports.push(eval_pack_case_report_from_trials(
            case,
            case_id,
            label,
            severity,
            split,
            blocking,
            harness_config_fingerprint.clone(),
            trials,
        ));
    }

    let mut ladder_reports = Vec::new();
    for ladder in &manifest.ladders {
        let mut ladder = ladder.clone();
        if ladder.base_dir.is_none() {
            ladder.base_dir = manifest.base_dir.clone();
        }
        ladder_reports.push(run_persona_eval_ladder(&ladder)?);
    }

    let stats_rows = reports
        .iter()
        .map(|report| report.stats_row.clone())
        .collect::<Vec<_>>();
    let stats = eval_pack_stats_report(&stats_rows);
    let case_total = reports.len();
    let ladder_total = ladder_reports.len();
    let total = case_total + ladder_total;
    let trial_count = reports.iter().map(|report| report.trial_count).sum();
    let case_blocking_failed = reports
        .iter()
        .filter(|report| report.blocking && report.reliability.status != "all-pass")
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
    let run_state = match ledger.as_ref() {
        Some(ledger) => ledger.finish(requested_cells, skipped_cells, executed_cells)?,
        None => EvalPackRunState {
            schema: EVAL_LEDGER_RUN_STATE_SCHEMA.to_string(),
            suite: manifest.id.clone(),
            model: eval_pack_manifest_model(manifest).unwrap_or_else(|| "unknown".to_string()),
            requested_cells,
            completed_cells: requested_cells,
            executed_cells: requested_cells,
            ..EvalPackRunState::default()
        },
    };
    Ok(EvalPackReport {
        pack_id: manifest.id.clone(),
        harness_config_fingerprint,
        pass: blocking_failed == 0,
        total,
        passed,
        failed: total.saturating_sub(passed),
        blocking_failed,
        warning_failed,
        informational_failed,
        trial_count,
        run_state,
        split: manifest.split.as_ref().map(|_| split_report),
        stats,
        stats_rows,
        cases: reports,
        ladders: ladder_reports,
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_eval_pack_live_verify_trial(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    case_id: &str,
    trial: usize,
    trial_count: usize,
    severity: &str,
    blocking: bool,
    base_dir: Option<&Path>,
    live_executor: &mut dyn EvalPackLiveExecutor,
) -> Result<EvalPackTrialReport, VmError> {
    let workspace = eval_pack_live_workspace(case, base_dir)?;
    let executor = case.executor.as_ref().or(manifest.executor.as_ref());
    let verify_command = case.verify_command.as_ref().ok_or_else(|| {
        VmError::Runtime(format!(
            "eval pack live-verify case '{case_id}' is missing verify_command"
        ))
    })?;
    let Some(executor) = executor else {
        return Err(VmError::Runtime(format!(
            "eval pack live-verify case '{case_id}' is missing executor"
        )));
    };

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut informational = Vec::new();
    let request_payload = eval_pack_live_executor_request(
        manifest,
        case,
        case_id,
        trial,
        trial_count,
        &workspace,
        base_dir,
    )?;
    let request = EvalPackLiveExecutorRequest {
        executor: executor.clone(),
        payload: request_payload,
        manifest_id: manifest.id.clone(),
        case: case.clone(),
        case_id: case_id.to_string(),
        trial,
        trials: trial_count,
        workspace: workspace.clone(),
        base_dir: base_dir.map(Path::to_path_buf),
    };
    let mut outcome = match live_executor.execute(request) {
        Ok(outcome) => outcome,
        Err(error) => {
            failures.push(format!("live executor failed: {error}"));
            EvalPackLiveVerifyOutcome::default()
        }
    };
    failures.append(&mut outcome.failures);
    warnings.append(&mut outcome.warnings);
    informational.append(&mut outcome.informational);
    if outcome.timed_out {
        failures.push("live executor timed out".to_string());
    }
    if live_outcome_verification(&outcome) == "FAIL" {
        failures.push("live executor reported verification FAIL".to_string());
    }

    let verify_output = run_eval_pack_command(
        verify_command,
        &workspace,
        None,
        DEFAULT_LIVE_VERIFY_TIMEOUT_SECONDS,
    );
    let verification_exit_code = match verify_output {
        Ok(output) => {
            let exit_code = output.exit_code;
            if output.timed_out {
                outcome.timed_out = true;
                failures.push("verify command timed out".to_string());
            }
            if exit_code != 0 {
                failures.push(format!(
                    "verify command exited {exit_code}{}",
                    command_failure_excerpt(&output)
                ));
            }
            if outcome.wall_time_seconds == 0.0 {
                outcome.wall_time_seconds = output.wall_time_seconds;
            }
            Some(exit_code)
        }
        Err(error) => {
            failures.push(format!("verify command failed: {error}"));
            None
        }
    };

    let produced_paths = normalized_live_produced_paths(case, &outcome);
    failures.extend(eval_pack_live_expected_path_failures(
        &workspace,
        &case.expected_output_paths,
    ));
    failures.extend(eval_pack_live_required_snippet_failures(
        &workspace,
        &produced_paths,
        &case.required_output_snippets,
    ));
    failures.extend(eval_pack_live_tool_budget_failures(
        &case.tool_budgets,
        &outcome.tool_call_summary,
    ));

    let mut report = eval_pack_trial_report(
        trial,
        severity,
        blocking,
        outcome
            .run_id
            .clone()
            .unwrap_or_else(|| format!("live:{case_id}:{trial}")),
        outcome
            .workflow_id
            .clone()
            .unwrap_or_else(|| "live-verify".to_string()),
        outcome
            .source_path
            .clone()
            .or_else(|| Some(workspace.display().to_string())),
        outcome.stage_count.unwrap_or_default(),
        outcome.timed_out,
        outcome.wall_time_seconds,
        outcome.cost_usd,
        failures,
        warnings,
        informational,
        None,
    );
    let outcome_verification = live_outcome_verification(&outcome);
    if report.failures.is_empty()
        && outcome_verification.eq_ignore_ascii_case("skip")
        && verification_exit_code.unwrap_or_default() == 0
    {
        report.verification = "skip".to_string();
    }
    report.verification_exit_code = verification_exit_code;
    report.produced_paths = produced_paths;
    report.tool_call_summary = outcome.tool_call_summary;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_eval_pack_run_trial(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    trial: usize,
    severity: &str,
    blocking: bool,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
    rubrics_by_id: &BTreeMap<&str, &EvalPackRubric>,
) -> Result<EvalPackTrialReport, VmError> {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let informational = Vec::new();
    let run = load_eval_pack_case_run(case, base_dir, fixture_base_dir, fixtures_by_id)?;
    let fixture =
        load_eval_pack_case_fixture(case, base_dir, fixture_base_dir, fixtures_by_id, &run)?;
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

    Ok(eval_pack_trial_report(
        trial,
        severity,
        blocking,
        run.id.clone(),
        run.workflow_id.clone(),
        eval_pack_case_source_path(case, base_dir, fixture_base_dir, fixtures_by_id),
        eval.stage_count,
        run.status.to_ascii_lowercase().contains("timeout"),
        run.usage
            .as_ref()
            .map(|usage| usage.total_duration_ms as f64 / 1000.0)
            .unwrap_or_default(),
        run.usage
            .as_ref()
            .map(|usage| usage.total_cost)
            .unwrap_or_default(),
        failures,
        warnings,
        informational,
        comparison,
    ))
}

#[allow(clippy::too_many_arguments)]
fn evaluate_eval_pack_friction_trial(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    trial: usize,
    severity: &str,
    blocking: bool,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
    rubrics_by_id: &BTreeMap<&str, &EvalPackRubric>,
) -> Result<EvalPackTrialReport, VmError> {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let informational = Vec::new();
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

    Ok(eval_pack_trial_report(
        trial,
        severity,
        blocking,
        "friction_events".to_string(),
        String::new(),
        eval_pack_case_friction_source_path(case, base_dir, fixture_base_dir, fixtures_by_id),
        events.len(),
        false,
        0.0,
        0.0,
        failures,
        warnings,
        informational,
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
mod live_tool_budget_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn per_tool_budget_counts_from_sequence_when_no_by_tool_map() {
        // The in-process coding-agent executor emits only
        // {total, rejected, sequence, successful} — no `by_tool` map.
        let summary = serde_json::json!({
            "total": 4,
            "rejected": 0,
            "sequence": ["read", "edit", "edit", "run"],
            "successful": ["read", "edit", "edit", "run"],
        });
        assert_eq!(live_tool_summary_count(&summary, "edit"), Some(2));
        assert_eq!(live_tool_summary_count(&summary, "read"), Some(1));
        assert_eq!(live_tool_summary_count(&summary, "delete"), Some(0));
        assert_eq!(live_tool_summary_count(&summary, "total"), Some(4));
    }

    #[test]
    fn per_tool_budget_is_enforced_against_sequence_only_summary() {
        let summary = serde_json::json!({
            "total": 3,
            "sequence": ["edit", "edit", "run"],
        });
        let budgets = BTreeMap::from([("edit".to_string(), 1usize)]);
        let failures = eval_pack_live_tool_budget_failures(&budgets, &summary);
        assert_eq!(failures.len(), 1, "edit budget of 1 must trip on 2 edits");
        assert!(failures[0].contains("edit"));

        let within = BTreeMap::from([("edit".to_string(), 2usize)]);
        assert!(eval_pack_live_tool_budget_failures(&within, &summary).is_empty());
    }

    #[test]
    fn explicit_by_tool_map_still_takes_precedence() {
        let summary = serde_json::json!({
            "total": 1,
            "byTool": {"edit": 1},
        });
        assert_eq!(live_tool_summary_count(&summary, "edit"), Some(1));
    }
}
