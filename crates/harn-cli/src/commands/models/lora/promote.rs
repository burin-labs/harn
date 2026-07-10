use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::cli::ModelsLoraPromoteArgs;

use super::render_embedded_lora_report;

const LORA_PROMOTE_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PROMOTE_PAYLOAD_JSON";
const LORA_PROMOTE_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PROMOTE_PAYLOAD_PRETTY";

pub(super) async fn promote(args: &ModelsLoraPromoteArgs) -> i32 {
    let report = match promotion_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if let Some(path) = args.out.as_deref() {
        if let Err(error) = write_receipt(path, &report) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    render_embedded_lora_report(
        &report,
        LORA_PROMOTE_PAYLOAD_ENV,
        LORA_PROMOTE_PAYLOAD_PRETTY_ENV,
        "models/lora_promote",
        args.json,
        "LoRA promote",
    )
    .await
}

fn promotion_report(args: &ModelsLoraPromoteArgs) -> Result<LoraPromotionReport, String> {
    let manifest = read_json(&args.manifest)?;
    let payload = unwrap_cli_envelope(&manifest);
    let evidence = promotion_evidence_contract(payload).ok_or_else(|| {
        format!(
            "{} does not contain promotion.evidence_contract or evaluation.evidence_contract",
            args.manifest.display()
        )
    })?;
    let not_applicable = parse_not_applicable(&args.not_applicable)?;
    let manifest_dir = args.manifest.parent().unwrap_or_else(|| Path::new("."));
    let required_cases = evidence
        .get("required_probe_cases")
        .and_then(Value::as_array)
        .ok_or_else(|| "promotion evidence contract is missing required_probe_cases".to_string())?;
    let mut cases = Vec::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut total_cost_usd = 0.0;
    for case in required_cases {
        let spec = probe_case_spec(case)?;
        let result = if let Some(reason) = not_applicable.get(&spec.case_id) {
            if spec.requirement.contains("not_applicable") {
                PromotionProbeResult {
                    case_id: spec.case_id.clone(),
                    requirement: spec.requirement.clone(),
                    expected: spec.expected.clone(),
                    receipt: spec.receipt.clone(),
                    status: "not_applicable".to_string(),
                    reason: reason.clone(),
                    summary_path: None,
                    per_case_path: None,
                    total_cases: 0,
                    passed_cases: 0,
                    pass_rate: None,
                    total_cost_usd: None,
                    failed_case_reasons: Vec::new(),
                }
            } else {
                errors.push(format!(
                    "probe case {} is required ({}) and cannot be marked not applicable",
                    spec.case_id, spec.requirement
                ));
                missing_probe_result(&spec, "invalid not-applicable override")
            }
        } else {
            collect_probe_result(&spec, evidence, manifest_dir, &args.probe_root)?
        };
        if let Some(cost) = result.total_cost_usd {
            total_cost_usd += cost;
        }
        if result.status == "missing" || result.status == "fail" {
            errors.push(format!(
                "probe case {} status={} reason={}",
                result.case_id, result.status, result.reason
            ));
        }
        cases.push(result);
    }
    for case_id in not_applicable.keys() {
        if !cases.iter().any(|case| &case.case_id == case_id) {
            warnings.push(format!(
                "not-applicable override for unknown probe case {case_id} was ignored"
            ));
        }
    }
    let totals = PromotionProbeTotals {
        required_cases: cases.len() as u64,
        passed: cases.iter().filter(|case| case.status == "pass").count() as u64,
        failed: cases.iter().filter(|case| case.status == "fail").count() as u64,
        missing: cases.iter().filter(|case| case.status == "missing").count() as u64,
        not_applicable: cases
            .iter()
            .filter(|case| case.status == "not_applicable")
            .count() as u64,
        total_cost_usd,
    };
    let ok = errors.is_empty();
    Ok(LoraPromotionReport {
        schema_version: 1,
        producer: "harn_models_lora_promote_v1".to_string(),
        ok,
        receipt_kind: "promotion_probe_matrix_receipt".to_string(),
        request: PromotionRequest {
            manifest: args.manifest.display().to_string(),
            probe_root: args.probe_root.display().to_string(),
            out: args.out.as_ref().map(|path| path.display().to_string()),
            check: args.check,
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
        },
        totals,
        cases,
        warnings,
        errors,
    })
}

fn collect_probe_result(
    spec: &ProbeCaseSpec,
    evidence: &Value,
    manifest_dir: &Path,
    probe_root: &Path,
) -> Result<PromotionProbeResult, String> {
    let candidates = summary_candidates(evidence, manifest_dir, probe_root, &spec.case_id);
    let Some(summary_path) = candidates.iter().find(|path| path.is_file()).cloned() else {
        return Ok(missing_probe_result(
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
    let status = if total_cases == 0 {
        "fail"
    } else if passed_cases == total_cases && failed_case_reasons.is_empty() {
        "pass"
    } else {
        "fail"
    };
    let reason = if status == "pass" {
        format!("{passed_cases}/{total_cases} probe cases passed")
    } else if total_cases == 0 {
        "summary selected zero probe cases".to_string()
    } else {
        format!("{passed_cases}/{total_cases} probe cases passed")
    };
    Ok(PromotionProbeResult {
        case_id: spec.case_id.clone(),
        requirement: spec.requirement.clone(),
        expected: spec.expected.clone(),
        receipt: spec.receipt.clone(),
        status: status.to_string(),
        reason,
        summary_path: Some(summary_path.display().to_string()),
        per_case_path: per_case_path_for_summary(&summary_path).is_file().then(|| {
            per_case_path_for_summary(&summary_path)
                .display()
                .to_string()
        }),
        total_cases,
        passed_cases,
        pass_rate,
        total_cost_usd,
        failed_case_reasons,
    })
}

fn missing_probe_result(spec: &ProbeCaseSpec, reason: &str) -> PromotionProbeResult {
    PromotionProbeResult {
        case_id: spec.case_id.clone(),
        requirement: spec.requirement.clone(),
        expected: spec.expected.clone(),
        receipt: spec.receipt.clone(),
        status: "missing".to_string(),
        reason: reason.to_string(),
        summary_path: None,
        per_case_path: None,
        total_cases: 0,
        passed_cases: 0,
        pass_rate: None,
        total_cost_usd: None,
        failed_case_reasons: Vec::new(),
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

fn parse_not_applicable(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut parsed = BTreeMap::new();
    for item in raw {
        let Some((case_id, reason)) = item.split_once('=') else {
            return Err(format!(
                "invalid --not-applicable `{item}`; expected CASE=REASON"
            ));
        };
        let case_id = case_id.trim();
        let reason = reason.trim();
        if case_id.is_empty() || reason.is_empty() {
            return Err(format!(
                "invalid --not-applicable `{item}`; case id and reason must be non-empty"
            ));
        }
        parsed.insert(case_id.to_string(), reason.to_string());
    }
    Ok(parsed)
}

fn promotion_evidence_contract(payload: &Value) -> Option<&Value> {
    payload
        .get("promotion")
        .and_then(|promotion| promotion.get("evidence_contract"))
        .or_else(|| {
            payload
                .get("evaluation")
                .and_then(|evaluation| evaluation.get("evidence_contract"))
        })
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

fn write_receipt(path: &Path, report: &LoraPromotionReport) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let mut file = std::fs::File::create(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    let body = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("failed to serialize promotion receipt: {error}"))?;
    file.write_all(&body)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    file.write_all(b"\n")
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

#[derive(Debug, Serialize)]
struct LoraPromotionReport {
    schema_version: u64,
    producer: String,
    ok: bool,
    receipt_kind: String,
    request: PromotionRequest,
    contract: PromotionContractSummary,
    totals: PromotionProbeTotals,
    cases: Vec<PromotionProbeResult>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PromotionRequest {
    manifest: String,
    probe_root: String,
    out: Option<String>,
    check: bool,
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
}

#[derive(Debug, Serialize)]
struct PromotionProbeTotals {
    required_cases: u64,
    passed: u64,
    failed: u64,
    missing: u64,
    not_applicable: u64,
    total_cost_usd: f64,
}

#[derive(Debug, Serialize)]
struct PromotionProbeResult {
    case_id: String,
    requirement: String,
    expected: String,
    receipt: String,
    status: String,
    reason: String,
    summary_path: Option<String>,
    per_case_path: Option<String>,
    total_cases: u64,
    passed_cases: u64,
    pass_rate: Option<f64>,
    total_cost_usd: Option<f64>,
    failed_case_reasons: Vec<String>,
}
