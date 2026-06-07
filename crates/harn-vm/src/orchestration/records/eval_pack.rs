//! Eval-suite manifest + eval-pack manifest loading, evaluation, replay-fixture comparison.

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

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
struct EvalPackLiveOutcome {
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

pub fn eval_ledger_read_report(
    options: Option<serde_json::Value>,
) -> Result<EvalLedgerReadReport, VmError> {
    let options = eval_ledger_options(options)?;
    let namespace = eval_ledger_namespace(&options);
    let topic = eval_ledger_topic(&namespace)?;
    let log = ensure_eval_ledger_event_log(None);
    let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &options))?;
    Ok(EvalLedgerReadReport { rows })
}

pub fn eval_ledger_append_rows_report(
    rows: serde_json::Value,
    options: Option<serde_json::Value>,
) -> Result<EvalLedgerAppendReport, VmError> {
    let options = eval_ledger_options(options)?;
    let namespace = eval_ledger_namespace(&options);
    let topic = eval_ledger_topic(&namespace)?;
    let provenance = eval_ledger_provenance(None, &options, None);
    let rows = parse_eval_ledger_rows(rows)?
        .into_iter()
        .map(|mut row| {
            normalize_eval_ledger_row(&mut row, &options, &provenance);
            row
        })
        .collect::<Vec<_>>();
    let log = ensure_eval_ledger_event_log(None);
    futures::executor::block_on(append_eval_ledger_rows(&log, &topic, rows))
}

pub fn eval_ledger_prior_commit_rows_report(
    options: serde_json::Value,
) -> Result<EvalLedgerPriorCommitReport, VmError> {
    let options = eval_ledger_options(Some(options))?;
    let namespace = eval_ledger_namespace(&options);
    let topic = eval_ledger_topic(&namespace)?;
    let log = ensure_eval_ledger_event_log(None);
    let mut read_options = options.clone();
    read_options.commit = None;
    read_options.case_fingerprint = None;
    read_options.harness_config_fingerprint = None;
    let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &read_options))?;
    Ok(prior_commit_report(rows, &options))
}

pub fn eval_ledger_resume_plan_report(
    manifest: &EvalPackManifest,
    options: Option<serde_json::Value>,
) -> Result<EvalLedgerResumePlan, VmError> {
    let split_report = validate_eval_pack_split(manifest)?;
    let harness_config_fingerprint = eval_pack_harness_config_fingerprint(manifest)?;
    let options = eval_ledger_options(options)?;
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let suite = options.suite.clone().unwrap_or_else(|| manifest.id.clone());
    let model = options
        .model
        .clone()
        .or_else(|| eval_pack_manifest_model(manifest))
        .unwrap_or_else(|| "unknown".to_string());
    let provenance = eval_ledger_provenance(base_dir, &options, Some(&manifest.metadata));
    let commit = options
        .commit
        .clone()
        .unwrap_or_else(|| provenance.commit.clone());
    let namespace = eval_pack_ledger_namespace(manifest, &options);
    let topic = eval_ledger_topic(&namespace)?;
    let log = ensure_eval_ledger_event_log(base_dir);
    let read_options = eval_pack_ledger_read_options(&suite, &model, &commit);
    let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &read_options))?;
    Ok(build_eval_ledger_resume_plan(
        manifest,
        &split_report,
        &rows,
        &suite,
        &model,
        &commit,
        &harness_config_fingerprint,
    ))
}

fn eval_ledger_options(value: Option<serde_json::Value>) -> Result<EvalLedgerOptions, VmError> {
    let mut options = match value {
        None | Some(serde_json::Value::Null) => EvalLedgerOptions::default(),
        Some(value) => serde_json::from_value(value)
            .map_err(|e| VmError::Runtime(format!("eval ledger options parse error: {e}")))?,
    };
    normalize_optional_string(&mut options.namespace);
    normalize_optional_string(&mut options.suite);
    normalize_optional_string(&mut options.model);
    normalize_optional_string(&mut options.split);
    normalize_optional_string(&mut options.commit);
    normalize_optional_string(&mut options.branch);
    normalize_optional_string(&mut options.case_name);
    normalize_optional_string(&mut options.case_fingerprint);
    normalize_optional_string(&mut options.harness_config_fingerprint);
    Ok(options)
}

fn normalize_optional_string(value: &mut Option<String>) {
    if value.as_deref().is_some_and(|text| text.trim().is_empty()) {
        *value = None;
    }
}

fn parse_eval_ledger_rows(value: serde_json::Value) -> Result<Vec<EvalLedgerRow>, VmError> {
    match value {
        serde_json::Value::Array(_) => serde_json::from_value(value)
            .map_err(|e| VmError::Runtime(format!("eval ledger rows parse error: {e}"))),
        serde_json::Value::Object(_) => serde_json::from_value(value)
            .map(|row| vec![row])
            .map_err(|e| VmError::Runtime(format!("eval ledger row parse error: {e}"))),
        _ => Err(VmError::Runtime(
            "eval ledger rows must be a row dict or list of row dicts".to_string(),
        )),
    }
}

fn eval_ledger_namespace(options: &EvalLedgerOptions) -> String {
    options
        .namespace
        .clone()
        .or_else(|| options.suite.clone())
        .unwrap_or_else(|| "default".to_string())
}

fn eval_pack_ledger_namespace(manifest: &EvalPackManifest, options: &EvalLedgerOptions) -> String {
    options
        .namespace
        .clone()
        .or_else(|| metadata_string(&manifest.metadata, &["ledger_namespace", "ledgerNamespace"]))
        .or_else(|| options.suite.clone())
        .unwrap_or_else(|| manifest.id.clone())
}

fn eval_pack_ledger_read_options(suite: &str, model: &str, commit: &str) -> EvalLedgerOptions {
    EvalLedgerOptions {
        suite: Some(suite.to_string()),
        model: Some(model.to_string()),
        commit: Some(commit.to_string()),
        ..EvalLedgerOptions::default()
    }
}

fn eval_ledger_topic(namespace: &str) -> Result<crate::event_log::Topic, VmError> {
    let safe_namespace = crate::event_log::sanitize_topic_component(namespace);
    crate::event_log::Topic::new(format!("{EVAL_LEDGER_TOPIC_PREFIX}.{safe_namespace}"))
        .map_err(eval_ledger_log_error)
}

fn ensure_eval_ledger_event_log(base_dir: Option<&Path>) -> Arc<crate::event_log::AnyEventLog> {
    if let Some(log) = crate::event_log::active_event_log() {
        return log;
    }
    if let Some(base_dir) = base_dir {
        if crate::event_log::install_lazy_default_for_base_dir(base_dir).is_ok() {
            if let Some(log) = crate::event_log::active_event_log() {
                return log;
            }
        }
    } else if let Ok(cwd) = std::env::current_dir() {
        if crate::event_log::install_lazy_default_for_base_dir(&cwd).is_ok() {
            if let Some(log) = crate::event_log::active_event_log() {
                return log;
            }
        }
    }
    crate::event_log::install_memory_for_current_thread(EVAL_LEDGER_QUEUE_DEPTH)
}

async fn read_eval_ledger_rows(
    log: &Arc<crate::event_log::AnyEventLog>,
    topic: &crate::event_log::Topic,
    options: &EvalLedgerOptions,
) -> Result<Vec<EvalLedgerRow>, VmError> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let batch = log
            .read_range(topic, cursor, EVAL_LEDGER_READ_BATCH_LIMIT)
            .await
            .map_err(eval_ledger_log_error)?;
        if batch.is_empty() {
            break;
        }
        for (event_id, event) in batch {
            cursor = Some(event_id);
            if let Some(row) = parse_eval_ledger_row(event_id, event) {
                if eval_ledger_row_matches(&row, options) {
                    rows.push(row);
                    if options.limit.is_some_and(|limit| rows.len() >= limit) {
                        return Ok(rows);
                    }
                }
            }
        }
    }
    Ok(rows)
}

async fn append_eval_ledger_rows(
    log: &Arc<crate::event_log::AnyEventLog>,
    topic: &crate::event_log::Topic,
    rows: Vec<EvalLedgerRow>,
) -> Result<EvalLedgerAppendReport, VmError> {
    let mut report = EvalLedgerAppendReport {
        appended: rows.len(),
        all_skipped: !rows.is_empty() && rows.iter().all(eval_ledger_row_is_skip),
        ..EvalLedgerAppendReport::default()
    };
    for row in rows {
        let identity = eval_ledger_row_identity(&row)?;
        let mut headers = BTreeMap::new();
        headers.insert(EVAL_LEDGER_IDENTITY_HEADER.to_string(), identity.clone());
        headers.insert("suite".to_string(), row.suite.clone());
        headers.insert("model".to_string(), row.model.clone());
        headers.insert("commit".to_string(), row.commit.clone());
        headers.insert("case_name".to_string(), row.case_name.clone());
        headers.insert("trial".to_string(), row.trial.to_string());
        let payload = serde_json::to_value(&row)
            .map_err(|e| VmError::Runtime(format!("eval ledger row encode error: {e}")))?;
        let outcome = log
            .append_idempotent_by_header(
                topic,
                EVAL_LEDGER_IDENTITY_HEADER,
                &identity,
                crate::event_log::LogEvent::new(EVAL_LEDGER_ROW_KIND, payload)
                    .with_headers(headers),
            )
            .await
            .map_err(eval_ledger_log_error)?;
        if outcome.inserted {
            report.inserted += 1;
        } else {
            report.duplicates += 1;
        }
        report.event_ids.push(outcome.event_id);
        if let Some(stored) = parse_eval_ledger_row(outcome.event_id, outcome.event) {
            report.rows.push(stored);
        }
    }
    log.flush().await.map_err(eval_ledger_log_error)?;
    Ok(report)
}

fn parse_eval_ledger_row(
    event_id: crate::event_log::EventId,
    event: crate::event_log::LogEvent,
) -> Option<EvalLedgerRow> {
    if event.kind != EVAL_LEDGER_ROW_KIND {
        return None;
    }
    let mut row: EvalLedgerRow = serde_json::from_value(event.payload).ok()?;
    if row.schema != EVAL_LEDGER_ROW_SCHEMA {
        return None;
    }
    row.event_id = Some(event_id);
    Some(row)
}

fn eval_ledger_row_matches(row: &EvalLedgerRow, options: &EvalLedgerOptions) -> bool {
    option_matches(options.suite.as_deref(), &row.suite)
        && option_matches(options.model.as_deref(), &row.model)
        && option_matches(options.commit.as_deref(), &row.commit)
        && option_matches(options.case_name.as_deref(), &row.case_name)
        && option_matches(options.case_fingerprint.as_deref(), &row.case_fingerprint)
        && option_matches(
            options.harness_config_fingerprint.as_deref(),
            &row.harness_config_fingerprint,
        )
        && match options.split.as_deref() {
            Some(expected) => row.split.as_deref() == Some(expected),
            None => true,
        }
}

fn option_matches(expected: Option<&str>, actual: &str) -> bool {
    expected.is_none_or(|expected| expected == actual)
}

fn normalize_eval_ledger_row(
    row: &mut EvalLedgerRow,
    options: &EvalLedgerOptions,
    provenance: &EvalLedgerProvenance,
) {
    if row.schema.is_empty() {
        row.schema = EVAL_LEDGER_ROW_SCHEMA.to_string();
    }
    if row.suite.is_empty() {
        row.suite = options
            .suite
            .clone()
            .unwrap_or_else(|| eval_ledger_namespace(options));
    }
    if row.model.is_empty() {
        row.model = options
            .model
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
    }
    if row.split.is_none() {
        row.split = options.split.clone();
    }
    if row.commit.is_empty() {
        row.commit = options
            .commit
            .clone()
            .unwrap_or_else(|| provenance.commit.clone());
    }
    if row.case_name.is_empty() {
        row.case_name = options
            .case_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| row.name.clone());
    }
    if row.name.is_empty() {
        row.name = row.case_name.clone();
    }
    if row.case_fingerprint.is_empty() {
        row.case_fingerprint = options.case_fingerprint.clone().unwrap_or_default();
    }
    if row.harness_config_fingerprint.is_empty() {
        row.harness_config_fingerprint = options
            .harness_config_fingerprint
            .clone()
            .unwrap_or_default();
    }
    if row.trial == 0 {
        row.trial = 1;
    }
    if row.trials == 0 {
        row.trials = 1;
    }
    if row.status.is_empty() {
        row.status = if row.passes > 0 {
            "PASS"
        } else if row.fails > 0 {
            "FAIL"
        } else {
            "skip"
        }
        .to_string();
    }
    if row.verification.is_empty() {
        row.verification = row.status.clone();
    }
    if row.passes + row.fails + row.skips == 0 {
        match row.status.to_ascii_uppercase().as_str() {
            "PASS" => row.passes = 1,
            "FAIL" => row.fails = 1,
            _ => row.skips = 1,
        }
    }
    if row.pass_rate == 0.0 && row.passes > 0 {
        row.pass_rate = row.passes as f64 / row.trials.max(1) as f64;
    }
    if row.provenance.commit.is_empty() {
        row.provenance.commit = row.commit.clone();
    }
    if row.provenance.branch.is_none() {
        row.provenance.branch = provenance.branch.clone();
    }
    if row.provenance.ts.is_empty() {
        row.provenance.ts = provenance.ts.clone();
    }
    if row.provenance.harn_version.is_empty() {
        row.provenance.harn_version = provenance.harn_version.clone();
    }
    if row.provenance.host.is_empty() {
        row.provenance.host = provenance.host.clone();
    }
}

fn eval_ledger_row_identity(row: &EvalLedgerRow) -> Result<String, VmError> {
    let material = serde_json::json!({
        "schema": EVAL_LEDGER_ROW_SCHEMA,
        "suite": row.suite,
        "model": row.model,
        "split": row.split,
        "commit": row.commit,
        "case_name": row.case_name,
        "case_fingerprint": row.case_fingerprint,
        "harness_config_fingerprint": row.harness_config_fingerprint,
        "trial": row.trial,
    });
    let bytes = serde_json::to_vec(&material)
        .map_err(|e| VmError::Runtime(format!("eval ledger identity encode error: {e}")))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn eval_ledger_row_is_skip(row: &EvalLedgerRow) -> bool {
    row.skipped || row.skips > 0 || row.status.eq_ignore_ascii_case("skip")
}

fn eval_ledger_provenance(
    base_dir: Option<&Path>,
    options: &EvalLedgerOptions,
    metadata: Option<&BTreeMap<String, serde_json::Value>>,
) -> EvalLedgerProvenance {
    let commit = options
        .commit
        .clone()
        .or_else(|| {
            metadata.and_then(|metadata| {
                metadata_string(metadata, &["commit", "git_commit", "source_commit"])
            })
        })
        .or_else(|| env_string(&["HARN_EVAL_COMMIT", "HARN_GIT_COMMIT", "GITHUB_SHA"]))
        .or_else(|| git_output(base_dir, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());
    let branch = options
        .branch
        .clone()
        .or_else(|| {
            metadata.and_then(|metadata| {
                metadata_string(metadata, &["branch", "git_branch", "source_branch"])
            })
        })
        .or_else(|| env_string(&["HARN_EVAL_BRANCH", "HARN_GIT_BRANCH", "GITHUB_REF_NAME"]))
        .or_else(|| git_output(base_dir, &["rev-parse", "--abbrev-ref", "HEAD"]));
    EvalLedgerProvenance {
        commit,
        branch,
        ts: now_rfc3339(),
        harn_version: crate::bytecode_cache::HARN_VERSION.to_string(),
        host: env_string(&["HOSTNAME", "COMPUTERNAME"]).unwrap_or_else(|| "unknown".to_string()),
    }
}

fn env_string(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn git_output(base_dir: Option<&Path>, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new("git");
    if let Some(base_dir) = base_dir {
        command.arg("-C").arg(base_dir);
    }
    let output = command.args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn metadata_string(
    metadata: &BTreeMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| json_value_string(metadata.get(*key)?))
}

fn json_value_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.trim().to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

fn eval_pack_manifest_model(manifest: &EvalPackManifest) -> Option<String> {
    metadata_string(&manifest.metadata, &["model", "provider_model", "route"])
        .or_else(|| {
            manifest
                .judge
                .as_ref()
                .and_then(|judge| judge.model.clone())
        })
        .or_else(|| {
            manifest
                .defaults
                .judge
                .as_ref()
                .and_then(|judge| judge.model.clone())
        })
}

fn prior_commit_report(
    rows: Vec<EvalLedgerRow>,
    options: &EvalLedgerOptions,
) -> EvalLedgerPriorCommitReport {
    let current_commit = options.commit.as_deref().unwrap_or_default();
    let mut fingerprint_mismatches = Vec::new();
    let mut candidates = Vec::new();
    let mut latest_event_by_commit = BTreeMap::<String, u64>::new();
    for row in rows {
        if row.commit == current_commit {
            continue;
        }
        if let Some(mismatch) = fingerprint_mismatch_for_row(&row, options) {
            fingerprint_mismatches.push(mismatch);
            continue;
        }
        let event_id = row.event_id.unwrap_or_default();
        latest_event_by_commit
            .entry(row.commit.clone())
            .and_modify(|existing| *existing = (*existing).max(event_id))
            .or_insert(event_id);
        candidates.push(row);
    }
    let selected_commit = latest_event_by_commit
        .iter()
        .max_by_key(|(_, event_id)| *event_id)
        .map(|(commit, _)| commit.clone());
    let rows = selected_commit
        .as_ref()
        .map(|commit| {
            candidates
                .into_iter()
                .filter(|row| &row.commit == commit)
                .collect()
        })
        .unwrap_or_default();
    EvalLedgerPriorCommitReport {
        commit: selected_commit,
        model: options.model.clone().unwrap_or_default(),
        split: options.split.clone(),
        rows,
        fingerprint_mismatches,
    }
}

fn fingerprint_mismatch_for_row(
    row: &EvalLedgerRow,
    options: &EvalLedgerOptions,
) -> Option<EvalLedgerFingerprintMismatch> {
    let expected_case = options.case_fingerprint.as_deref();
    let expected_harness = options.harness_config_fingerprint.as_deref();
    let case_mismatch = expected_case.is_some_and(|expected| expected != row.case_fingerprint);
    let harness_mismatch =
        expected_harness.is_some_and(|expected| expected != row.harness_config_fingerprint);
    if !(case_mismatch || harness_mismatch) {
        return None;
    }
    Some(EvalLedgerFingerprintMismatch {
        case_name: row.case_name.clone(),
        split: row.split.clone(),
        commit: row.commit.clone(),
        trial: row.trial,
        case_fingerprint: row.case_fingerprint.clone(),
        harness_config_fingerprint: row.harness_config_fingerprint.clone(),
        expected_case_fingerprint: expected_case.unwrap_or_default().to_string(),
        expected_harness_config_fingerprint: expected_harness.unwrap_or_default().to_string(),
    })
}

fn build_eval_ledger_resume_plan(
    manifest: &EvalPackManifest,
    split_report: &EvalPackSplitValidationReport,
    rows: &[EvalLedgerRow],
    suite: &str,
    model: &str,
    commit: &str,
    harness_config_fingerprint: &str,
) -> EvalLedgerResumePlan {
    let split_by_case = split_by_case_id(split_report);
    let mut cells = Vec::new();
    let mut fingerprint_refusals = Vec::new();
    let mut skipped_cells = 0usize;
    for (index, case) in manifest.cases.iter().enumerate() {
        let case_id = eval_pack_case_id(case, index);
        let split = split_by_case.get(&case_id).cloned();
        let trial_count = case.trials.unwrap_or(manifest.trials);
        for trial in 1..=trial_count {
            let matching = ledger_rows_for_cell(
                rows,
                suite,
                model,
                split.as_deref(),
                commit,
                &case_id,
                trial,
            );
            let exact = matching
                .iter()
                .copied()
                .filter(|row| {
                    row.case_fingerprint == case.case_fingerprint
                        && row.harness_config_fingerprint == harness_config_fingerprint
                })
                .max_by_key(|row| row.event_id.unwrap_or_default());
            if let Some(row) = exact {
                skipped_cells += 1;
                cells.push(EvalLedgerResumeCell {
                    case_name: case_id.clone(),
                    split: split.clone(),
                    trial,
                    status: "skip".to_string(),
                    reason: "matching ledger row".to_string(),
                    event_id: row.event_id,
                });
                continue;
            }
            let mut refused = false;
            for row in matching {
                if row.case_fingerprint != case.case_fingerprint
                    || row.harness_config_fingerprint != harness_config_fingerprint
                {
                    fingerprint_refusals.push(EvalLedgerFingerprintMismatch {
                        case_name: case_id.clone(),
                        split: split.clone(),
                        commit: row.commit.clone(),
                        trial,
                        case_fingerprint: row.case_fingerprint.clone(),
                        harness_config_fingerprint: row.harness_config_fingerprint.clone(),
                        expected_case_fingerprint: case.case_fingerprint.clone(),
                        expected_harness_config_fingerprint: harness_config_fingerprint.to_string(),
                    });
                    refused = true;
                }
            }
            cells.push(EvalLedgerResumeCell {
                case_name: case_id.clone(),
                split: split.clone(),
                trial,
                status: "run".to_string(),
                reason: if refused {
                    "fingerprint mismatch".to_string()
                } else {
                    "missing ledger row".to_string()
                },
                event_id: None,
            });
        }
    }
    let requested_cells = cells.len();
    let remaining_cells = requested_cells.saturating_sub(skipped_cells);
    EvalLedgerResumePlan {
        schema: EVAL_LEDGER_RESUME_PLAN_SCHEMA.to_string(),
        suite: suite.to_string(),
        model: model.to_string(),
        commit: commit.to_string(),
        harness_config_fingerprint: harness_config_fingerprint.to_string(),
        requested_cells,
        completed_cells: skipped_cells,
        skipped_cells,
        remaining_cells,
        all_skipped: requested_cells > 0 && remaining_cells == 0,
        fingerprint_refusals,
        cells,
    }
}

fn ledger_rows_for_cell<'a>(
    rows: &'a [EvalLedgerRow],
    suite: &str,
    model: &str,
    split: Option<&str>,
    commit: &str,
    case_name: &str,
    trial: usize,
) -> Vec<&'a EvalLedgerRow> {
    rows.iter()
        .filter(|row| {
            row.suite == suite
                && row.model == model
                && row.split.as_deref() == split
                && row.commit == commit
                && row.case_name == case_name
                && row.trial == trial
        })
        .collect()
}

fn eval_ledger_log_error(error: crate::event_log::LogError) -> VmError {
    VmError::Runtime(format!("eval ledger: event log: {error}"))
}

impl EvalPackLedgerRun {
    fn start(
        manifest: &EvalPackManifest,
        base_dir: Option<&Path>,
        options: Option<serde_json::Value>,
    ) -> Result<Self, VmError> {
        let options = eval_ledger_options(options)?;
        let suite = options.suite.clone().unwrap_or_else(|| manifest.id.clone());
        let model = options
            .model
            .clone()
            .or_else(|| eval_pack_manifest_model(manifest))
            .unwrap_or_else(|| "unknown".to_string());
        let provenance = eval_ledger_provenance(base_dir, &options, Some(&manifest.metadata));
        let commit = options
            .commit
            .clone()
            .unwrap_or_else(|| provenance.commit.clone());
        let namespace = eval_pack_ledger_namespace(manifest, &options);
        let topic = eval_ledger_topic(&namespace)?;
        let log = ensure_eval_ledger_event_log(base_dir);
        let read_options = eval_pack_ledger_read_options(&suite, &model, &commit);
        let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &read_options))?;
        Ok(Self {
            log,
            topic,
            rows,
            suite,
            model,
            commit,
            branch: provenance.branch.clone(),
            provenance,
            inserted: 0,
            duplicates: 0,
            fingerprint_refusals: Vec::new(),
        })
    }

    fn replay_row_for_cell(
        &mut self,
        case_id: &str,
        split: Option<&str>,
        trial: usize,
        case_fingerprint: &str,
        harness_config_fingerprint: &str,
    ) -> Option<EvalLedgerRow> {
        let matching = ledger_rows_for_cell(
            &self.rows,
            &self.suite,
            &self.model,
            split,
            &self.commit,
            case_id,
            trial,
        );
        let exact = matching
            .iter()
            .copied()
            .filter(|row| {
                row.case_fingerprint == case_fingerprint
                    && row.harness_config_fingerprint == harness_config_fingerprint
            })
            .max_by_key(|row| row.event_id.unwrap_or_default())
            .cloned();
        if exact.is_some() {
            return exact;
        }
        for row in matching {
            if row.case_fingerprint != case_fingerprint
                || row.harness_config_fingerprint != harness_config_fingerprint
            {
                self.fingerprint_refusals
                    .push(EvalLedgerFingerprintMismatch {
                        case_name: case_id.to_string(),
                        split: split.map(str::to_string),
                        commit: row.commit.clone(),
                        trial,
                        case_fingerprint: row.case_fingerprint.clone(),
                        harness_config_fingerprint: row.harness_config_fingerprint.clone(),
                        expected_case_fingerprint: case_fingerprint.to_string(),
                        expected_harness_config_fingerprint: harness_config_fingerprint.to_string(),
                    });
            }
        }
        None
    }

    fn append_trial_row(&mut self, row: EvalLedgerRow) -> Result<(), VmError> {
        let report = futures::executor::block_on(append_eval_ledger_rows(
            &self.log,
            &self.topic,
            vec![row],
        ))?;
        self.inserted += report.inserted;
        self.duplicates += report.duplicates;
        self.rows.extend(report.rows);
        Ok(())
    }

    fn finish(
        &self,
        requested_cells: usize,
        skipped_cells: usize,
        executed_cells: usize,
    ) -> Result<EvalPackRunState, VmError> {
        let remaining_cells = requested_cells.saturating_sub(skipped_cells + executed_cells);
        let mut state = EvalPackRunState {
            schema: EVAL_LEDGER_RUN_STATE_SCHEMA.to_string(),
            suite: self.suite.clone(),
            model: self.model.clone(),
            commit: self.commit.clone(),
            branch: self.branch.clone(),
            requested_cells,
            completed_cells: skipped_cells + executed_cells,
            skipped_cells,
            executed_cells,
            remaining_cells,
            ledger_rows_inserted: self.inserted,
            ledger_rows_duplicate: self.duplicates,
            fingerprint_refusals: self.fingerprint_refusals.len(),
            all_skipped: requested_cells > 0 && skipped_cells == requested_cells,
            heartbeat_event_id: None,
        };
        let event_id = self.append_run_state(&state)?;
        state.heartbeat_event_id = Some(event_id);
        Ok(state)
    }

    fn append_run_state(&self, state: &EvalPackRunState) -> Result<u64, VmError> {
        let payload = serde_json::to_value(state)
            .map_err(|e| VmError::Runtime(format!("eval run-state encode error: {e}")))?;
        let event_id = futures::executor::block_on(self.log.append(
            &self.topic,
            crate::event_log::LogEvent::new(EVAL_LEDGER_RUN_STATE_KIND, payload),
        ))
        .map_err(eval_ledger_log_error)?;
        futures::executor::block_on(self.log.flush()).map_err(eval_ledger_log_error)?;
        Ok(event_id)
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

fn run_eval_pack_command(
    spec: &EvalPackCommandSpec,
    default_cwd: &Path,
    stdin_payload: Option<&serde_json::Value>,
    default_timeout_seconds: f64,
) -> Result<EvalPackCommandOutput, VmError> {
    let timeout_seconds = command_timeout(spec).unwrap_or(default_timeout_seconds);
    let timeout = timeout_seconds
        .is_finite()
        .then_some(timeout_seconds)
        .filter(|seconds| *seconds > 0.0)
        .ok_or_else(|| {
            VmError::Runtime(
                "eval pack command timeout must be a positive finite number of seconds".to_string(),
            )
        })?;
    let mut command = eval_pack_command(spec)?;
    command
        .current_dir(command_cwd(spec, default_cwd))
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_command_env(spec, &mut command);

    let started = crate::clock_mock::leak_audit::instant_now("eval_pack.command.started");
    let mut child = command
        .spawn()
        .map_err(|e| VmError::Runtime(format!("eval pack command spawn failed: {e}")))?;
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    });
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    });

    let mut stdin_error = None;
    if let Some(payload) = stdin_payload {
        match child.stdin.take() {
            Some(mut stdin) => {
                if let Err(error) = serde_json::to_writer(&mut stdin, payload) {
                    stdin_error = Some(format!("eval pack command stdin encode failed: {error}"));
                } else if let Err(error) = stdin.write_all(b"\n") {
                    stdin_error = Some(format!("eval pack command stdin write failed: {error}"));
                }
            }
            None => {
                stdin_error = Some("eval pack command stdin pipe was unavailable".to_string());
            }
        }
    }

    let timeout = Duration::from_secs_f64(timeout);
    let status = if stdin_error.is_some() {
        let _ = child.kill();
        let _ = child.wait();
        None
    } else {
        match child
            .wait_timeout(timeout)
            .map_err(|e| VmError::Runtime(format!("eval pack command wait failed: {e}")))?
        {
            Some(status) => Some(status),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    };
    if let Some(error) = stdin_error {
        let _ = join_command_reader(stdout_reader, "stdout")?;
        let _ = join_command_reader(stderr_reader, "stderr")?;
        return Err(VmError::Runtime(error));
    }
    let wall_time_seconds = started.elapsed().as_secs_f64();
    let stdout = join_command_reader(stdout_reader, "stdout")?;
    let stderr = join_command_reader(stderr_reader, "stderr")?;
    let timed_out = status.is_none();
    let exit_code = status.and_then(|status| status.code()).unwrap_or(-1) as i64;
    Ok(EvalPackCommandOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
        wall_time_seconds,
    })
}

fn eval_pack_command(spec: &EvalPackCommandSpec) -> Result<Command, VmError> {
    match spec {
        EvalPackCommandSpec::Shell(command) => shell_command(command),
        EvalPackCommandSpec::Argv(argv) => argv_command(argv),
        EvalPackCommandSpec::Object(object) => {
            if let Some(command) = object.command.as_deref() {
                shell_command(command)
            } else {
                argv_command(&object.argv)
            }
        }
    }
}

fn shell_command(command: &str) -> Result<Command, VmError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(VmError::Runtime(
            "eval pack shell command must not be empty".to_string(),
        ));
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        Ok(cmd)
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        Ok(cmd)
    }
}

fn argv_command(argv: &[String]) -> Result<Command, VmError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(VmError::Runtime(
            "eval pack argv command must include a program".to_string(),
        ));
    };
    if program.trim().is_empty() {
        return Err(VmError::Runtime(
            "eval pack argv command program must not be empty".to_string(),
        ));
    }
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

fn command_cwd(spec: &EvalPackCommandSpec, default_cwd: &Path) -> PathBuf {
    let cwd = match spec {
        EvalPackCommandSpec::Object(EvalPackCommandObject { cwd: Some(cwd), .. }) => cwd.as_str(),
        _ => return default_cwd.to_path_buf(),
    };
    let path = PathBuf::from(cwd);
    if path.is_absolute() {
        path
    } else {
        default_cwd.join(path)
    }
}

fn apply_command_env(spec: &EvalPackCommandSpec, command: &mut Command) {
    if let EvalPackCommandSpec::Object(object) = spec {
        command.envs(&object.env);
    }
}

fn command_timeout(spec: &EvalPackCommandSpec) -> Option<f64> {
    match spec {
        EvalPackCommandSpec::Object(object) => object.timeout_seconds,
        _ => None,
    }
}

fn join_command_reader(
    reader: Option<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stream: &str,
) -> Result<String, VmError> {
    let Some(reader) = reader else {
        return Ok(String::new());
    };
    let bytes = reader
        .join()
        .map_err(|_| VmError::Runtime(format!("eval pack command {stream} reader panicked")))?
        .map_err(|e| VmError::Runtime(format!("eval pack command {stream} read failed: {e}")))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn eval_pack_live_workspace(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
) -> Result<PathBuf, VmError> {
    let workspace = case
        .workspace
        .as_deref()
        .or(case.project.as_deref())
        .ok_or_else(|| {
            VmError::Runtime("eval pack live-verify case is missing workspace".to_string())
        })?;
    let workspace = resolve_manifest_path(base_dir, workspace);
    if !workspace.is_dir() {
        return Err(VmError::Runtime(format!(
            "eval pack live-verify workspace does not exist: {}",
            workspace.display()
        )));
    }
    Ok(workspace)
}

fn eval_pack_live_executor_request(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    case_id: &str,
    trial: usize,
    trial_count: usize,
    workspace: &Path,
    base_dir: Option<&Path>,
) -> Result<serde_json::Value, VmError> {
    Ok(serde_json::json!({
        "schema": LIVE_EXECUTOR_REQUEST_SCHEMA,
        "manifest": {
            "id": &manifest.id,
            "base_dir": base_dir.map(|path| path.display().to_string()),
            "metadata": &manifest.metadata,
        },
        "case": {
            "id": case_id,
            "name": &case.name,
            "task": &case.task,
            "workspace": workspace.display().to_string(),
            "project": &case.project,
            "verify_command": command_spec_json(case.verify_command.as_ref())?,
            "expected_output_paths": &case.expected_output_paths,
            "required_output_snippets": &case.required_output_snippets,
            "tool_budgets": &case.tool_budgets,
            "metadata": &case.metadata,
            "case_fingerprint": &case.case_fingerprint,
        },
        "trial": trial,
        "trials": trial_count,
    }))
}

fn command_spec_json(spec: Option<&EvalPackCommandSpec>) -> Result<serde_json::Value, VmError> {
    match spec {
        Some(spec) => serde_json::to_value(spec)
            .map_err(|e| VmError::Runtime(format!("eval pack command encode failed: {e}"))),
        None => Ok(serde_json::Value::Null),
    }
}

fn live_outcome_from_executor_output(
    output: EvalPackCommandOutput,
    failures: &mut Vec<String>,
) -> EvalPackLiveOutcome {
    let mut outcome = parse_live_outcome_stdout(&output.stdout).unwrap_or_else(|error| {
        if !output.stdout.trim().is_empty() {
            failures.push(error);
        }
        EvalPackLiveOutcome::default()
    });
    if output.timed_out {
        outcome.timed_out = true;
    }
    if output.exit_code != 0 {
        failures.push(format!(
            "live executor exited {}{}",
            output.exit_code,
            command_failure_excerpt(&output)
        ));
    }
    if outcome.wall_time_seconds == 0.0 {
        outcome.wall_time_seconds = output.wall_time_seconds;
    }
    outcome
}

fn parse_live_outcome_stdout(stdout: &str) -> Result<EvalPackLiveOutcome, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(EvalPackLiveOutcome::default());
    }
    serde_json::from_str(trimmed)
        .or_else(|_| {
            trimmed
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .ok_or_else(|| serde_json::Error::io(std::io::ErrorKind::UnexpectedEof.into()))
                .and_then(|line| serde_json::from_str(line.trim()))
        })
        .map_err(|error| format!("live executor stdout did not contain a JSON outcome: {error}"))
}

fn live_outcome_verification(outcome: &EvalPackLiveOutcome) -> String {
    if let Some(verification) = outcome.verification.as_deref() {
        return normalize_live_verification(verification);
    }
    if outcome.timed_out {
        return "FAIL".to_string();
    }
    if let Some(exit_code) = outcome.verification_exit_code {
        return if exit_code == 0 { "PASS" } else { "FAIL" }.to_string();
    }
    if let Some(passed) = outcome.passed {
        return if passed { "PASS" } else { "FAIL" }.to_string();
    }
    "PASS".to_string()
}

fn normalize_live_verification(verification: &str) -> String {
    match verification.trim().to_ascii_lowercase().as_str() {
        "pass" | "passed" | "success" | "ok" => "PASS".to_string(),
        "skip" | "skipped" => "skip".to_string(),
        _ => "FAIL".to_string(),
    }
}

fn command_failure_excerpt(output: &EvalPackCommandOutput) -> String {
    let stderr = compact_output_excerpt(&output.stderr);
    if !stderr.is_empty() {
        return format!("; stderr: {stderr}");
    }
    let stdout = compact_output_excerpt(&output.stdout);
    if stdout.is_empty() {
        String::new()
    } else {
        format!("; stdout: {stdout}")
    }
}

fn compact_output_excerpt(output: &str) -> String {
    let compact = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let max_chars = 240;
    if compact.chars().count() > max_chars {
        format!("{}...", compact.chars().take(max_chars).collect::<String>())
    } else {
        compact
    }
}

fn normalized_live_produced_paths(
    case: &EvalPackCase,
    outcome: &EvalPackLiveOutcome,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for path in outcome
        .produced_paths
        .iter()
        .chain(case.expected_output_paths.iter())
    {
        if !path.trim().is_empty() && seen.insert(path.clone()) {
            paths.push(path.clone());
        }
    }
    paths
}

fn eval_pack_live_expected_path_failures(workspace: &Path, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| {
            let resolved = resolve_manifest_path(Some(workspace), path);
            (!resolved.exists()).then(|| {
                format!(
                    "expected output path does not exist: {}",
                    resolved.display()
                )
            })
        })
        .collect()
}

fn eval_pack_live_required_snippet_failures(
    workspace: &Path,
    paths: &[String],
    snippets: &[String],
) -> Vec<String> {
    let readable_outputs = paths
        .iter()
        .map(|path| resolve_manifest_path(Some(workspace), path))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    snippets
        .iter()
        .filter(|snippet| !snippet.is_empty())
        .filter_map(|snippet| {
            let found = readable_outputs.iter().any(|path| {
                std::fs::read_to_string(path)
                    .map(|content| content.contains(snippet))
                    .unwrap_or(false)
            });
            (!found).then(|| format!("required output snippet not found: {snippet:?}"))
        })
        .collect()
}

fn eval_pack_live_tool_budget_failures(
    budgets: &BTreeMap<String, usize>,
    summary: &serde_json::Value,
) -> Vec<String> {
    budgets
        .iter()
        .filter_map(|(name, limit)| {
            let count = live_tool_summary_count(summary, name)?;
            (count > *limit)
                .then(|| format!("tool budget {name} exceeded: {count} calls > {limit}"))
        })
        .collect()
}

fn live_tool_summary_count(summary: &serde_json::Value, name: &str) -> Option<usize> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return None;
    }
    if normalized == "total" {
        return json_usize_from_keys(summary, &["total", "calls", "tool_calls", "toolCalls"]);
    }
    json_usize_from_keys(summary, &[normalized])
        .or_else(|| {
            summary
                .get("by_tool")
                .or_else(|| summary.get("byTool"))
                .and_then(|value| json_usize_from_keys(value, &[normalized]))
        })
        .or_else(|| {
            summary
                .get("tools")
                .and_then(|value| json_usize_from_keys(value, &[normalized]))
        })
}

fn json_usize_from_keys(value: &serde_json::Value, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(json_value_usize)
}

fn json_value_usize(value: &serde_json::Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| value.as_i64().and_then(|value| usize::try_from(value).ok()))
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
    evaluate_eval_pack_manifest_inner(manifest, false, None)
}

pub fn evaluate_eval_pack_manifest_resumable(
    manifest: &EvalPackManifest,
    ledger_options: Option<serde_json::Value>,
) -> Result<EvalPackReport, VmError> {
    evaluate_eval_pack_manifest_inner(manifest, true, ledger_options)
}

fn evaluate_eval_pack_manifest_inner(
    manifest: &EvalPackManifest,
    ledger_enabled: bool,
    ledger_options: Option<serde_json::Value>,
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
    let request = eval_pack_live_executor_request(
        manifest,
        case,
        case_id,
        trial,
        trial_count,
        &workspace,
        base_dir,
    )?;
    let executor_output = run_eval_pack_command(
        executor,
        &workspace,
        Some(&request),
        DEFAULT_LIVE_EXECUTOR_TIMEOUT_SECONDS,
    );
    let mut outcome = match executor_output {
        Ok(output) => live_outcome_from_executor_output(output, &mut failures),
        Err(error) => {
            failures.push(format!("live executor failed: {error}"));
            EvalPackLiveOutcome::default()
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
fn eval_pack_trial_report(
    trial: usize,
    severity: &str,
    blocking: bool,
    run_id: String,
    workflow_id: String,
    source_path: Option<String>,
    stage_count: usize,
    timed_out: bool,
    wall_time_seconds: f64,
    cost_usd: f64,
    mut failures: Vec<String>,
    mut warnings: Vec<String>,
    mut informational: Vec<String>,
    comparison: Option<RunDiffReport>,
) -> EvalPackTrialReport {
    let verification = if failures.is_empty() { "PASS" } else { "FAIL" }.to_string();
    let pass = failures.is_empty() || !blocking;
    if !failures.is_empty() && !blocking {
        if severity == "warning" {
            warnings.append(&mut failures);
        } else {
            informational.append(&mut failures);
        }
    }
    EvalPackTrialReport {
        trial,
        verification,
        verification_exit_code: None,
        pass,
        blocking,
        run_id,
        workflow_id,
        source_path,
        stage_count,
        failures,
        warnings,
        informational,
        comparison,
        timed_out,
        wall_time_seconds,
        cost_usd,
        produced_paths: Vec::new(),
        tool_call_summary: serde_json::Value::Null,
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_ledger_row_from_trial(
    case: &EvalPackCase,
    case_id: &str,
    split: Option<String>,
    suite: &str,
    model: &str,
    commit: &str,
    provenance: &EvalLedgerProvenance,
    harness_config_fingerprint: &str,
    trial: &EvalPackTrialReport,
) -> EvalLedgerRow {
    let passes = usize::from(trial.verification == "PASS");
    let fails = usize::from(trial.verification == "FAIL");
    let skips = usize::from(trial.verification.eq_ignore_ascii_case("skip"));
    let timeouts = usize::from(trial.timed_out);
    EvalLedgerRow {
        schema: EVAL_LEDGER_ROW_SCHEMA.to_string(),
        suite: suite.to_string(),
        model: model.to_string(),
        split,
        commit: commit.to_string(),
        case_name: case_id.to_string(),
        name: case_id.to_string(),
        case_fingerprint: case.case_fingerprint.clone(),
        harness_config_fingerprint: harness_config_fingerprint.to_string(),
        trial: trial.trial,
        trials: 1,
        passes,
        fails,
        skips,
        timeouts,
        pass_rate: passes as f64,
        status: trial.verification.clone(),
        verification: trial.verification.clone(),
        skipped: false,
        wall_time_seconds: trial.wall_time_seconds,
        cost_usd: trial.cost_usd,
        mean_wall_time_seconds: trial.wall_time_seconds,
        total_cost_usd: trial.cost_usd,
        run_id: trial.run_id.clone(),
        workflow_id: trial.workflow_id.clone(),
        source_path: trial.source_path.clone(),
        trial_report: Some(trial.clone()),
        provenance: provenance.clone(),
        metadata: case.metadata.clone(),
        ..EvalLedgerRow::default()
    }
}

fn eval_pack_trial_report_from_ledger_row(
    row: &EvalLedgerRow,
    blocking: bool,
) -> EvalPackTrialReport {
    if let Some(mut report) = row.trial_report.clone() {
        report.trial = row.trial;
        return report;
    }
    let mut failures = Vec::new();
    let verification = if row.verification.is_empty() {
        row.status.clone()
    } else {
        row.verification.clone()
    };
    if verification == "FAIL" {
        failures.push("ledger row recorded a failed trial".to_string());
    }
    EvalPackTrialReport {
        trial: row.trial,
        verification: verification.clone(),
        verification_exit_code: None,
        pass: verification != "FAIL" || !blocking,
        blocking,
        run_id: row.run_id.clone(),
        workflow_id: row.workflow_id.clone(),
        source_path: row.source_path.clone(),
        stage_count: 0,
        failures,
        warnings: Vec::new(),
        informational: Vec::new(),
        comparison: None,
        timed_out: row.timeouts > 0,
        wall_time_seconds: row.wall_time_seconds,
        cost_usd: row.cost_usd,
        produced_paths: Vec::new(),
        tool_call_summary: serde_json::Value::Null,
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_pack_case_report_from_trials(
    case: &EvalPackCase,
    case_id: String,
    label: String,
    severity: String,
    split: Option<String>,
    blocking: bool,
    harness_config_fingerprint: String,
    trials: Vec<EvalPackTrialReport>,
) -> EvalPackCaseReport {
    let reliability = eval_pack_reliability_report(&trials);
    let stats_row = eval_pack_stats_row(
        case,
        &case_id,
        &harness_config_fingerprint,
        split.clone(),
        &trials,
        &reliability,
    );
    let first = trials.first();
    let pass = if blocking {
        reliability.status == "all-pass"
    } else {
        true
    };
    let failures = prefixed_trial_messages(&trials, |trial| &trial.failures);
    let warnings = prefixed_trial_messages(&trials, |trial| &trial.warnings);
    let informational = prefixed_trial_messages(&trials, |trial| &trial.informational);
    EvalPackCaseReport {
        id: case_id,
        label,
        severity,
        split,
        case_fingerprint: case.case_fingerprint.clone(),
        harness_config_fingerprint,
        pass,
        blocking,
        run_id: first.map(|trial| trial.run_id.clone()).unwrap_or_default(),
        workflow_id: first
            .map(|trial| trial.workflow_id.clone())
            .unwrap_or_default(),
        source_path: first.and_then(|trial| trial.source_path.clone()),
        stage_count: first.map(|trial| trial.stage_count).unwrap_or_default(),
        trial_count: trials.len(),
        total_stage_count: trials.iter().map(|trial| trial.stage_count).sum(),
        reliability,
        stats_row,
        comparison: first.and_then(|trial| trial.comparison.clone()),
        trials,
        failures,
        warnings,
        informational,
    }
}

fn prefixed_trial_messages<F>(trials: &[EvalPackTrialReport], messages: F) -> Vec<String>
where
    F: Fn(&EvalPackTrialReport) -> &Vec<String>,
{
    let include_prefix = trials.len() > 1;
    let mut out = Vec::new();
    for trial in trials {
        for message in messages(trial) {
            if include_prefix {
                out.push(format!("trial {}: {message}", trial.trial));
            } else {
                out.push(message.clone());
            }
        }
    }
    out
}

fn eval_pack_reliability_report(trials: &[EvalPackTrialReport]) -> EvalPackReliabilityReport {
    let passes = trials
        .iter()
        .filter(|trial| trial.verification == "PASS")
        .count();
    let fails = trials
        .iter()
        .filter(|trial| trial.verification == "FAIL")
        .count();
    let skips = trials
        .iter()
        .filter(|trial| trial.verification.eq_ignore_ascii_case("skip"))
        .count();
    let timeouts = trials.iter().filter(|trial| trial.timed_out).count();
    let decided = passes + fails;
    let majority = if passes > 0 && fails > 0 {
        Some(if passes >= fails { "PASS" } else { "FAIL" }.to_string())
    } else {
        None
    };
    let status = if decided == 0 {
        "no-decision"
    } else if fails == 0 {
        "all-pass"
    } else if passes == 0 {
        "all-fail"
    } else {
        "flaky"
    };
    EvalPackReliabilityReport {
        status: status.to_string(),
        trials: trials.len(),
        passes,
        fails,
        skips,
        timeouts,
        decided,
        pass_rate: if trials.is_empty() {
            0.0
        } else {
            passes as f64 / trials.len() as f64
        },
        majority,
    }
}

fn eval_pack_stats_row(
    case: &EvalPackCase,
    case_id: &str,
    harness_config_fingerprint: &str,
    split: Option<String>,
    trials: &[EvalPackTrialReport],
    reliability: &EvalPackReliabilityReport,
) -> EvalPackStatsRow {
    let wall_times = trials
        .iter()
        .map(|trial| trial.wall_time_seconds)
        .collect::<Vec<_>>();
    let costs = trials
        .iter()
        .map(|trial| trial.cost_usd)
        .collect::<Vec<_>>();
    let group = case
        .metadata
        .get("group")
        .or_else(|| case.metadata.get("language"))
        .or_else(|| case.metadata.get("bucket"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    EvalPackStatsRow {
        name: case_id.to_string(),
        case_name: case_id.to_string(),
        case_fingerprint: case.case_fingerprint.clone(),
        harness_config_fingerprint: harness_config_fingerprint.to_string(),
        group,
        split,
        trials: trials.len(),
        passes: reliability.passes,
        fails: reliability.fails,
        skips: reliability.skips,
        timeouts: reliability.timeouts,
        pass_rate: reliability.pass_rate,
        status: match reliability.status.as_str() {
            "all-pass" => "PASS",
            "all-fail" => "FAIL",
            "flaky" => "FLAKY",
            _ => "skip",
        }
        .to_string(),
        majority: reliability.majority.clone(),
        wall_time_seconds: mean(&wall_times),
        cost_usd: costs.iter().sum(),
        mean_wall_time_seconds: mean(&wall_times),
        stdev_wall_time_seconds: stdev(&wall_times),
        total_cost_usd: costs.iter().sum(),
    }
}

fn eval_pack_stats_report(rows: &[EvalPackStatsRow]) -> EvalPackStatsReport {
    EvalPackStatsReport {
        macro_pass_at_1: macro_pass_at_1(rows),
        reliability: eval_pack_reliability_breakdown(rows),
    }
}

fn macro_pass_at_1(rows: &[EvalPackStatsRow]) -> f64 {
    let decided = rows
        .iter()
        .filter(|row| row.passes + row.fails > 0)
        .collect::<Vec<_>>();
    if decided.is_empty() {
        return 0.0;
    }
    decided.iter().map(|row| row.pass_rate).sum::<f64>() / decided.len() as f64
}

fn eval_pack_reliability_breakdown(rows: &[EvalPackStatsRow]) -> EvalPackReliabilityBreakdown {
    let total_cases = rows.len();
    let all_pass_cases = rows
        .iter()
        .filter(|row| row.passes > 0 && row.fails == 0)
        .count();
    let flaky_cases = rows
        .iter()
        .filter(|row| row.passes > 0 && row.fails > 0)
        .count();
    let all_fail_cases = rows
        .iter()
        .filter(|row| row.passes == 0 && row.fails > 0)
        .count();
    let no_decision_cases = rows
        .iter()
        .filter(|row| row.passes + row.fails == 0)
        .count();
    EvalPackReliabilityBreakdown {
        all_pass_cases,
        flaky_cases,
        all_fail_cases,
        no_decision_cases,
        total_cases,
        all_pass_fraction: rate(all_pass_cases, total_cases),
        flaky_fraction: rate(flaky_cases, total_cases),
        all_fail_fraction: rate(all_fail_cases, total_cases),
        no_decision_fraction: rate(no_decision_cases, total_cases),
    }
}

fn split_by_case_id(report: &EvalPackSplitValidationReport) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (partition, cases) in &report.partitions {
        for case_id in cases {
            out.insert(case_id.clone(), partition.clone());
        }
    }
    out
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn stdev(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = mean(values);
    let variance = values
        .iter()
        .map(|value| {
            let diff = value - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

fn rate(count: usize, denom: usize) -> f64 {
    if denom == 0 {
        0.0
    } else {
        count as f64 / denom as f64
    }
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
