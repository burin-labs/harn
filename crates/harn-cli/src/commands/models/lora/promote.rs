use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::cli::ModelsLoraPromoteArgs;

use super::{render_embedded_lora_report, run_embedded_lora_report, sha256_file};

const LORA_PROMOTE_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PROMOTE_PAYLOAD_JSON";
const LORA_PROMOTE_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PROMOTE_PAYLOAD_PRETTY";

pub(super) async fn promote(args: &ModelsLoraPromoteArgs) -> i32 {
    let bundle = match promotion_evidence_bundle(args) {
        Ok(bundle) => bundle,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if let Some(path) = args.out.as_deref() {
        if let Err(error) = prepare_receipt_parent(path) {
            eprintln!("error: {error}");
            return 1;
        }
        let outcome = match run_embedded_lora_report(
            &bundle,
            LORA_PROMOTE_PAYLOAD_ENV,
            LORA_PROMOTE_PAYLOAD_PRETTY_ENV,
            "models/lora_promote",
            true,
            "LoRA promote",
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        };
        match final_report_from_json_envelope(&outcome.stdout) {
            Ok(report) => {
                if let Err(error) = write_receipt_value(path, &report) {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
            Err(error) => {
                if !outcome.stderr.is_empty() {
                    eprint!("{}", outcome.stderr);
                }
                eprintln!("error: failed to capture Harn promotion receipt: {error}");
                return 1;
            }
        }
    }
    render_embedded_lora_report(
        &bundle,
        LORA_PROMOTE_PAYLOAD_ENV,
        LORA_PROMOTE_PAYLOAD_PRETTY_ENV,
        "models/lora_promote",
        args.json,
        "LoRA promote",
    )
    .await
}

fn promotion_evidence_bundle(
    args: &ModelsLoraPromoteArgs,
) -> Result<LoraPromotionEvidenceBundle, String> {
    let train_receipt = read_json(&args.train_receipt)?;
    let payload = unwrap_cli_envelope(&train_receipt);
    require_executed_train_receipt(payload, &args.train_receipt)?;
    let evidence = promotion_evidence_contract(payload).ok_or_else(|| {
        format!(
            "{} does not contain promotion.evidence_contract",
            args.train_receipt.display()
        )
    })?;
    let manifest_dir = args
        .train_receipt
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let required_cases = evidence
        .get("required_probe_cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "promotion evidence contract is missing required_probe_cases".to_string())?;
    let mut cases = Vec::new();
    let mut warnings = Vec::new();
    let trainer_identity = trainer_identity_check(evidence);
    let trainer_environment = trainer_environment_check(evidence);
    if (!trainer_identity.promotable || !trainer_environment.promotable)
        && args
            .trainer_provenance_exception
            .as_deref()
            .map(str::trim)
            .is_some_and(|exception| !exception.is_empty())
    {
        warnings.push("trainer provenance promotion exception supplied".to_string());
    }
    for case in required_cases {
        let spec = probe_case_spec(case)?;
        cases.push(collect_probe_evidence(
            "adapter",
            &spec,
            evidence,
            manifest_dir,
            &args.probe_root,
        )?);
        if let Some(base_probe_root) = args.base_probe_root.as_deref() {
            cases.push(collect_probe_evidence(
                "base",
                &spec,
                evidence,
                manifest_dir,
                base_probe_root,
            )?);
        } else {
            cases.push(missing_probe_evidence(
                "base",
                &spec,
                "base route probe root was not supplied",
            ));
        }
    }
    Ok(LoraPromotionEvidenceBundle {
        schema_version: 1,
        producer: "harn_models_lora_promote_bridge_v2".to_string(),
        request: PromotionRequest {
            train_receipt: args.train_receipt.display().to_string(),
            probe_root: args.probe_root.display().to_string(),
            base_probe_root: args
                .base_probe_root
                .as_ref()
                .map(|path| path.display().to_string()),
            out: args.out.as_ref().map(|path| path.display().to_string()),
            check: args.check,
            trainer_provenance_exception: args.trainer_provenance_exception.clone(),
        },
        contract: PromotionContractSummary {
            schema_version: value_u64(evidence, "schema_version"),
            promotion_id: value_string(evidence, "promotion_id"),
            lora_contract_id: value_string(evidence, "lora_contract_id"),
            eval_dataset: value_string(evidence, "eval_dataset"),
            minimum_trials: value_u64(evidence, "minimum_trials"),
            base_route: evidence.get("base_route").cloned().unwrap_or(Value::Null),
            adapter_route: evidence
                .get("adapter_route")
                .cloned()
                .unwrap_or(Value::Null),
            trainer_identity,
            trainer_environment,
        },
        evidence_cases: cases,
        warnings,
    })
}

fn collect_probe_evidence(
    route_role: &str,
    spec: &ProbeCaseSpec,
    evidence: &Value,
    manifest_dir: &Path,
    probe_root: &Path,
) -> Result<PromotionProbeEvidence, String> {
    let candidates = summary_candidates(evidence, manifest_dir, probe_root, &spec.case_id);
    let Some(summary_path) = candidates.iter().find(|path| path.is_file()).cloned() else {
        return Ok(missing_probe_evidence(
            route_role,
            spec,
            "summary.json was not found under the probe root or contract template path",
        ));
    };
    let summary = read_json(&summary_path)?;
    let total_cases = value_u64(&summary, "total_cases");
    let passed_cases = value_u64(&summary, "passed_cases");
    let pass_rate = summary.get("pass_rate").and_then(Value::as_f64);
    let total_cost_usd = summary.get("total_cost_usd").and_then(Value::as_f64);
    let failed_case_reasons = failed_case_reasons(&summary);
    let per_case_path = per_case_path_for_summary(&summary_path);
    let per_case_metrics = if per_case_path.is_file() {
        per_case_metrics(&per_case_path)?
    } else {
        PerCaseMetrics::default()
    };
    Ok(PromotionProbeEvidence {
        route_role: route_role.to_string(),
        case_id: spec.case_id.clone(),
        requirement: spec.requirement.clone(),
        expected: spec.expected.clone(),
        receipt: spec.receipt.clone(),
        present: true,
        missing_reason: String::new(),
        summary_path: Some(summary_path.display().to_string()),
        summary_sha256: Some(format!("sha256:{}", sha256_file(&summary_path)?)),
        per_case_path: per_case_path
            .is_file()
            .then(|| per_case_path.display().to_string()),
        per_case_sha256: per_case_path
            .is_file()
            .then(|| sha256_file(&per_case_path))
            .transpose()?
            .map(|hash| format!("sha256:{hash}")),
        total_cases,
        passed_cases,
        pass_rate,
        total_cost_usd,
        failed_case_reasons,
        per_case_count: per_case_metrics.count,
        per_case_passed_count: per_case_metrics.passed_count,
        per_case_ids: per_case_metrics.ids,
    })
}

fn missing_probe_evidence(
    route_role: &str,
    spec: &ProbeCaseSpec,
    reason: &str,
) -> PromotionProbeEvidence {
    PromotionProbeEvidence {
        route_role: route_role.to_string(),
        case_id: spec.case_id.clone(),
        requirement: spec.requirement.clone(),
        expected: spec.expected.clone(),
        receipt: spec.receipt.clone(),
        present: false,
        missing_reason: reason.to_string(),
        summary_path: None,
        summary_sha256: None,
        per_case_path: None,
        per_case_sha256: None,
        total_cases: 0,
        passed_cases: 0,
        pass_rate: None,
        total_cost_usd: None,
        failed_case_reasons: Vec::new(),
        per_case_count: 0,
        per_case_passed_count: 0,
        per_case_ids: Vec::new(),
    }
}

fn summary_candidates(
    evidence: &Value,
    manifest_dir: &Path,
    probe_root: &Path,
    case_id: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(probe_root.join(case_id).join("summary.json"));
    if probe_root.is_relative() {
        candidates.push(
            manifest_dir
                .join(probe_root)
                .join(case_id)
                .join("summary.json"),
        );
    }
    if let Some(template_path) = template_summary_path(evidence, case_id) {
        candidates.push(template_path.clone());
        if template_path.is_relative() {
            candidates.push(manifest_dir.join(template_path));
        }
    }
    dedupe_paths(candidates)
}

fn template_summary_path(evidence: &Value, case_id: &str) -> Option<PathBuf> {
    evidence
        .get("probe_command_templates")?
        .as_array()?
        .iter()
        .find(|template| value_string(template, "case_id") == case_id)?
        .get("summary_path")?
        .as_str()
        .map(PathBuf::from)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let key = path.display().to_string();
        if seen.insert(key) {
            deduped.push(path);
        }
    }
    deduped
}

fn failed_case_reasons(summary: &Value) -> Vec<String> {
    summary
        .get("cases")
        .and_then(Value::as_array)
        .map(|cases| {
            cases
                .iter()
                .filter(|case| {
                    case.get("passed")
                        .and_then(Value::as_bool)
                        .is_some_and(|passed| !passed)
                })
                .map(|case| {
                    let id = value_string(case, "id");
                    let reason = value_string(case, "reason");
                    if reason.is_empty() {
                        id
                    } else {
                        format!("{id}: {reason}")
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn per_case_path_for_summary(summary_path: &Path) -> PathBuf {
    summary_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("per_case.jsonl")
}

fn probe_case_spec(value: &Value) -> Result<ProbeCaseSpec, String> {
    let case_id = value_string(value, "id");
    if case_id.is_empty() {
        return Err("promotion probe case is missing id".to_string());
    }
    Ok(ProbeCaseSpec {
        case_id,
        requirement: value_string(value, "requirement"),
        expected: value_string(value, "expected"),
        receipt: value_string(value, "receipt"),
    })
}

fn promotion_evidence_contract(payload: &Value) -> Option<&Value> {
    payload
        .get("promotion")
        .and_then(|promotion| promotion.get("evidence_contract"))
}

fn trainer_identity_check(evidence: &Value) -> TrainerIdentityPromotionSummary {
    let candidate = evidence.get("trainer_identity");
    let Some(candidate) = candidate else {
        return TrainerIdentityPromotionSummary {
            status: "missing".to_string(),
            promotable: false,
            expected: Value::Null,
            observed: Value::Null,
            errors: vec!["trainer identity check is missing".to_string()],
        };
    };
    let status = value_string(candidate, "status");
    let promotable = candidate
        .get("promotable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let errors = candidate
        .get("errors")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut errors = errors;
    if !promotable && errors.is_empty() {
        errors.push("trainer identity is not promotable".to_string());
    }
    TrainerIdentityPromotionSummary {
        status,
        promotable,
        expected: candidate.get("expected").cloned().unwrap_or(Value::Null),
        observed: candidate.get("observed").cloned().unwrap_or(Value::Null),
        errors,
    }
}

fn trainer_environment_check(evidence: &Value) -> TrainerEnvironmentPromotionSummary {
    let Some(candidate) = evidence.get("trainer_environment") else {
        return TrainerEnvironmentPromotionSummary {
            status: "missing".to_string(),
            promotable: false,
            attestation: Value::Null,
            errors: vec!["trainer environment check is missing".to_string()],
        };
    };
    let status = value_string(candidate, "status");
    let promotable = candidate
        .get("promotable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut errors = candidate
        .get("errors")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !promotable && errors.is_empty() {
        errors.push("trainer environment is not promotable".to_string());
    }
    TrainerEnvironmentPromotionSummary {
        status,
        promotable,
        attestation: candidate.get("attestation").cloned().unwrap_or(Value::Null),
        errors,
    }
}

fn require_executed_train_receipt(payload: &Value, path: &Path) -> Result<(), String> {
    let producer = value_string(payload, "producer");
    let mode = value_string(payload, "mode");
    let request_execute = payload
        .get("request")
        .and_then(|request| request.get("execute"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let backend_execute = payload
        .get("backend")
        .and_then(|backend| backend.get("execute"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let backend_exit_code = payload
        .get("backend")
        .and_then(|backend| backend.get("exit_code"))
        .and_then(Value::as_i64);
    let backend_status = payload
        .get("backend")
        .and_then(|backend| backend.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if producer != "harn_models_lora_train_v1"
        || mode != "execute"
        || !request_execute
        || !backend_execute
        || backend_exit_code != Some(0)
        || !backend_status.starts_with("completed")
    {
        return Err(format!(
            "{} is not a finalized `harn models lora train --execute` receipt",
            path.display()
        ));
    }
    Ok(())
}

fn unwrap_cli_envelope(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str::<Value>(&raw)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn prepare_receipt_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    Ok(())
}

fn final_report_from_json_envelope(stdout: &str) -> Result<Value, String> {
    let envelope = serde_json::from_str::<Value>(stdout)
        .map_err(|error| format!("failed to parse Harn JSON envelope: {error}"))?;
    if envelope.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return envelope
            .get("data")
            .cloned()
            .ok_or_else(|| "successful envelope did not contain data".to_string());
    }
    envelope
        .get("error")
        .and_then(|error| error.get("details"))
        .cloned()
        .ok_or_else(|| "failure envelope did not contain error.details".to_string())
}

fn write_receipt_value(path: &Path, report: &Value) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("failed to serialize promotion receipt: {error}"))?;
    std::fs::write(path, [body, b"\n".to_vec()].concat())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn value_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn value_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

#[derive(Debug)]
struct ProbeCaseSpec {
    case_id: String,
    requirement: String,
    expected: String,
    receipt: String,
}

fn per_case_metrics(path: &Path) -> Result<PerCaseMetrics, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut ids = Vec::new();
    let mut passed_count = 0_u64;
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed).map_err(|error| {
            format!(
                "failed to parse {} line {} as JSON: {error}",
                path.display(),
                index + 1
            )
        })?;
        ids.push(value_string(&value, "id"));
        if value
            .get("passed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            passed_count += 1;
        }
    }
    Ok(PerCaseMetrics {
        count: ids.len() as u64,
        passed_count,
        ids,
    })
}

#[derive(Debug, Default)]
struct PerCaseMetrics {
    count: u64,
    passed_count: u64,
    ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct LoraPromotionEvidenceBundle {
    schema_version: u64,
    producer: String,
    request: PromotionRequest,
    contract: PromotionContractSummary,
    evidence_cases: Vec<PromotionProbeEvidence>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PromotionRequest {
    train_receipt: String,
    probe_root: String,
    base_probe_root: Option<String>,
    out: Option<String>,
    check: bool,
    trainer_provenance_exception: Option<String>,
}

#[derive(Debug, Serialize)]
struct PromotionContractSummary {
    schema_version: u64,
    promotion_id: String,
    lora_contract_id: String,
    eval_dataset: String,
    minimum_trials: u64,
    base_route: Value,
    adapter_route: Value,
    trainer_identity: TrainerIdentityPromotionSummary,
    trainer_environment: TrainerEnvironmentPromotionSummary,
}

#[derive(Debug, Serialize)]
struct TrainerIdentityPromotionSummary {
    status: String,
    promotable: bool,
    expected: Value,
    observed: Value,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TrainerEnvironmentPromotionSummary {
    status: String,
    promotable: bool,
    attestation: Value,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PromotionProbeEvidence {
    route_role: String,
    case_id: String,
    requirement: String,
    expected: String,
    receipt: String,
    present: bool,
    missing_reason: String,
    summary_path: Option<String>,
    summary_sha256: Option<String>,
    per_case_path: Option<String>,
    per_case_sha256: Option<String>,
    total_cases: u64,
    passed_cases: u64,
    pass_rate: Option<f64>,
    total_cost_usd: Option<f64>,
    failed_case_reasons: Vec<String>,
    per_case_count: u64,
    per_case_passed_count: u64,
    per_case_ids: Vec<String>,
}
