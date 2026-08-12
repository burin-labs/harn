//! One provenance-bound model review over a validated run report.

use harn_clock::{Clock, RealClock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use crate::llm::{execute_llm_call, extract_llm_options, vm_value_to_json};
use crate::value::{VmError, VmValue};

use super::{
    build_run_report, read_checked_run_report_bytes, validate_run_report, RunReport,
    RunReportRequest, ViewProducer,
};

pub const RUN_REVIEW_SCHEMA: &str = "harn.run_review.v1";
pub const RUN_REVIEW_SCHEMA_VERSION: u32 = 1;
pub const RUN_REVIEW_EVIDENCE_SCHEMA: &str = "harn.run_review_evidence.v1";
pub const MAX_RUN_REVIEW_INPUT_TOKENS: i64 = 48_000;
const MAX_PROJECTED_ARRAY_ITEMS: usize = 32;
const MAX_PROJECTED_STRING_BYTES: usize = 2_048;
pub const DEFAULT_RUN_REVIEW_RUBRIC: &str = "Assess the run using only the supplied run report. Judge whether the run completed its stated work, coordinated reliably, exposed material failures, and preserved enough evidence to support the verdict. Prefer a limitation over an unsupported claim.";

#[derive(Clone, Debug)]
pub enum RunReviewInput {
    Report {
        path: PathBuf,
        /// Empty for a trusted local CLI call. Remote adapters must provide
        /// every root from which the review may read.
        allowed_roots: Vec<PathBuf>,
    },
    RunRecord(RunReportRequest),
}

#[derive(Clone, Debug)]
pub struct RunReviewRequest {
    pub input: RunReviewInput,
    pub rubric: String,
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunReviewState {
    Located,
    Projected,
    Validated,
    Reviewing,
    Reviewed,
    Invalid,
    Failed,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewLifecycleReceipt {
    pub state: RunReviewState,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewLifecycle {
    pub state: RunReviewState,
    pub receipts: Vec<RunReviewLifecycleReceipt>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewProvenance {
    pub report_hash: String,
    pub rubric_hash: String,
    pub model_route: RunReviewModelRoute,
    pub evidence_projection: RunReviewEvidenceProjection,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewEvidenceProjection {
    pub schema: String,
    pub hash: String,
    pub source_bytes: usize,
    pub projected_bytes: usize,
    pub omissions: Vec<RunReviewEvidenceOmission>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewEvidenceOmission {
    pub report_pointer: String,
    pub kind: String,
    pub original_units: usize,
    pub included_units: usize,
    pub omitted_units: usize,
    pub omitted_hash: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewModelRoute {
    pub selector: String,
    pub provider: String,
    pub model: String,
    pub tier: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunReviewVerdict {
    Pass,
    Concerns,
    Fail,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunReviewFinding {
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub evidence_pointers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunReviewAction {
    pub priority: String,
    pub action: String,
    #[serde(default)]
    pub evidence_pointers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewLimitation {
    pub code: String,
    pub message: String,
    pub evidence_pointer: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct RunReviewUsage {
    pub duration_ms: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_hit_ratio: Option<f64>,
    pub cache_visibility: Option<String>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RunReview {
    pub schema: String,
    pub schema_version: u32,
    pub producer: ViewProducer,
    pub idempotency_key: String,
    pub provenance: RunReviewProvenance,
    pub lifecycle: RunReviewLifecycle,
    pub verdict: RunReviewVerdict,
    pub confidence: f64,
    pub summary: String,
    pub findings: Vec<RunReviewFinding>,
    pub limitations: Vec<RunReviewLimitation>,
    pub actions: Vec<RunReviewAction>,
    pub usage: RunReviewUsage,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunReviewError {
    pub schema: String,
    pub message: String,
    pub lifecycle: RunReviewLifecycle,
}

impl std::fmt::Display for RunReviewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RunReviewError {}

#[derive(Clone, Debug, Deserialize)]
struct ModelRunReview {
    verdict: RunReviewVerdict,
    confidence: f64,
    summary: String,
    findings: Vec<RunReviewFinding>,
    actions: Vec<RunReviewAction>,
}

pub async fn review_run_report(request: RunReviewRequest) -> Result<RunReview, RunReviewError> {
    review_run_report_with_clock(request, &RealClock::new()).await
}

async fn review_run_report_with_clock(
    request: RunReviewRequest,
    clock: &dyn Clock,
) -> Result<RunReview, RunReviewError> {
    let mut lifecycle = LifecycleRecorder::new(clock);
    let report = match request.input {
        RunReviewInput::Report {
            path,
            allowed_roots,
        } => {
            let bytes = read_checked_run_report_bytes(&path, &allowed_roots)
                .map_err(|error| lifecycle.invalid(error.to_string()))?;
            lifecycle.advance(RunReviewState::Located);
            serde_json::from_slice::<RunReport>(&bytes)
                .map_err(|error| lifecycle.invalid(format!("parse run report: {error}")))?
        }
        RunReviewInput::RunRecord(report_request) => {
            let report = build_run_report(report_request)
                .await
                .map_err(|error| lifecycle.invalid(format!("build run report: {error}")))?;
            lifecycle.advance(RunReviewState::Located);
            report
        }
    };
    lifecycle.advance(RunReviewState::Projected);
    validate_run_report(&report)
        .map_err(|error| lifecycle.invalid(format!("validate run report: {error}")))?;
    lifecycle.advance(RunReviewState::Validated);

    let report_value = serde_json::to_value(&report)
        .map_err(|error| lifecycle.invalid(format!("encode run report: {error}")))?;
    let rubric = request.rubric;
    if rubric.trim().is_empty() {
        return Err(lifecycle.invalid("run review rubric must not be empty".to_string()));
    }
    let report_hash = report.projection.hash.clone();
    let rubric_hash = sha256_prefixed(rubric.as_bytes());
    let source_bytes = crate::canonical_json::to_vec(&report_value).len();
    let (evidence, evidence_projection) = build_evidence_projection(&report_value, source_bytes);
    let prompt = build_prompt(&evidence, &evidence_projection, &rubric);
    let system = build_system_prompt();
    let estimated_input_tokens = crate::llm::estimate_text_tokens(&prompt)
        .saturating_add(crate::llm::estimate_text_tokens(&system));
    if estimated_input_tokens > MAX_RUN_REVIEW_INPUT_TOKENS {
        return Err(lifecycle.invalid(
            format!(
                "the deterministic run review projection is estimated at {estimated_input_tokens} tokens, above the {MAX_RUN_REVIEW_INPUT_TOKENS}-token limit; the report cannot be reviewed within the bounded one-call budget"
            ),
        ));
    }
    let options = build_llm_options(request.model.as_deref());
    let options_dict = options
        .as_dict()
        .cloned()
        .ok_or_else(|| lifecycle.invalid("invalid run review LLM options".to_string()))?;
    let extracted = extract_llm_options(&[
        VmValue::string(prompt),
        VmValue::string(system),
        options.clone(),
    ])
    .map_err(|error| lifecycle.failed(vm_error_message(error)))?;
    let selector = request.model.unwrap_or_else(|| "small".to_string());
    let route = RunReviewModelRoute {
        selector,
        provider: extracted.provider.clone(),
        model: extracted.model.clone(),
        tier: crate::llm_config::model_tier(&extracted.model),
    };
    let idempotency_key = idempotency_key(&report_hash, &rubric_hash, &route);

    lifecycle.advance(RunReviewState::Reviewing);
    let started_ms = clock.monotonic_ms();
    let response = execute_llm_call(None, extracted, Some(options_dict), None, None)
        .await
        .map_err(|error| lifecycle.failed(vm_error_message(error)))?;
    let response_dict = response.as_dict().ok_or_else(|| {
        lifecycle.failed("run review model response was not an object".to_string())
    })?;
    let data = response_dict.get("data").ok_or_else(|| {
        lifecycle.failed("run review model response did not contain structured data".to_string())
    })?;
    let mut review: ModelRunReview = serde_json::from_value(vm_value_to_json(data))
        .map_err(|error| lifecycle.failed(format!("decode run review model response: {error}")))?;
    normalize_model_review(&mut review, &report_value)
        .map_err(|message| lifecycle.failed(message))?;
    let mut limitations = report_limitations(&report);
    limitations.extend(projection_limitations(&evidence_projection));
    let duration_ms = clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64;
    let usage = usage_from_response(response_dict, duration_ms);
    let lifecycle = lifecycle.finish(RunReviewState::Reviewed);

    Ok(RunReview {
        schema: RUN_REVIEW_SCHEMA.to_string(),
        schema_version: RUN_REVIEW_SCHEMA_VERSION,
        producer: ViewProducer::default(),
        idempotency_key,
        provenance: RunReviewProvenance {
            report_hash,
            rubric_hash,
            model_route: route,
            evidence_projection,
        },
        lifecycle,
        verdict: review.verdict,
        confidence: review.confidence,
        summary: review.summary,
        findings: review.findings,
        limitations,
        actions: review.actions,
        usage,
    })
}

fn build_llm_options(model: Option<&str>) -> VmValue {
    let mut options = serde_json::json!({
        "provider": "auto",
        "model_tier": "small",
        "max_tokens": 2048,
        "output": {"schema": model_review_schema(), "validation": "error"},
        "schema_retries": 0
    });
    if let Some(model) = model {
        options
            .as_object_mut()
            .expect("object")
            .remove("model_tier");
        options["model"] = Value::String(model.to_string());
    }
    crate::stdlib::json_to_vm_value(&options)
}

fn model_review_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "confidence", "summary", "findings", "actions"],
        "properties": {
            "verdict": {"type": "string", "enum": ["pass", "concerns", "fail"]},
            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
            "summary": {"type": "string"},
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["severity", "title", "detail", "evidence_pointers"],
                    "properties": {
                        "severity": {"type": "string", "enum": ["blocking", "warning", "info"]},
                        "title": {"type": "string"},
                        "detail": {"type": "string"},
                        "evidence_pointers": {"type": "array", "minItems": 1, "items": {"type": "string"}}
                    }
                }
            },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["priority", "action", "evidence_pointers"],
                    "properties": {
                        "priority": {"type": "string", "enum": ["now", "next", "later"]},
                        "action": {"type": "string"},
                        "evidence_pointers": {"type": "array", "items": {"type": "string"}}
                    }
                }
            }
        }
    })
}

fn build_evidence_projection(
    report: &Value,
    source_bytes: usize,
) -> (Value, RunReviewEvidenceProjection) {
    let mut omissions = Vec::new();
    let evidence = project_evidence_value(report, "", &mut omissions);
    let encoded = crate::canonical_json::to_vec(&evidence);
    let receipt = RunReviewEvidenceProjection {
        schema: RUN_REVIEW_EVIDENCE_SCHEMA.to_string(),
        hash: sha256_prefixed(&encoded),
        source_bytes,
        projected_bytes: encoded.len(),
        omissions,
    };
    (evidence, receipt)
}

#[cfg(test)]
pub(crate) fn build_evidence_projection_for_test(
    report: &Value,
) -> (Value, RunReviewEvidenceProjection) {
    build_evidence_projection(report, crate::canonical_json::to_vec(report).len())
}

fn project_evidence_value(
    value: &Value,
    report_pointer: &str,
    omissions: &mut Vec<RunReviewEvidenceOmission>,
) -> Value {
    match value {
        Value::Array(items) if items.len() > MAX_PROJECTED_ARRAY_ITEMS => {
            project_evidence_array(items, report_pointer, omissions)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    project_evidence_value(
                        item,
                        &child_pointer(report_pointer, &index.to_string()),
                        omissions,
                    )
                })
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        project_evidence_value(
                            value,
                            &child_pointer(report_pointer, key),
                            omissions,
                        ),
                    )
                })
                .collect(),
        ),
        Value::String(text) if text.len() > MAX_PROJECTED_STRING_BYTES => {
            project_evidence_string(text, report_pointer, omissions)
        }
        _ => value.clone(),
    }
}

fn project_evidence_array(
    items: &[Value],
    report_pointer: &str,
    omissions: &mut Vec<RunReviewEvidenceOmission>,
) -> Value {
    let edge = MAX_PROJECTED_ARRAY_ITEMS / 2;
    let selected: Vec<usize> = (0..edge)
        .chain(items.len().saturating_sub(edge)..items.len())
        .collect();
    let omitted = &items[edge..items.len() - edge];
    let omission = RunReviewEvidenceOmission {
        report_pointer: report_pointer.to_string(),
        kind: "array_items".to_string(),
        original_units: items.len(),
        included_units: selected.len(),
        omitted_units: omitted.len(),
        omitted_hash: sha256_prefixed(&crate::canonical_json::to_vec(&Value::Array(
            omitted.to_vec(),
        ))),
    };
    omissions.push(omission.clone());
    let projected = selected
        .into_iter()
        .map(|index| {
            let pointer = child_pointer(report_pointer, &index.to_string());
            serde_json::json!({
                "report_pointer": pointer,
                "value": project_evidence_value(&items[index], &pointer, omissions),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "_harn_review_projection": omission,
        "items": projected,
    })
}

#[expect(
    clippy::string_slice,
    reason = "prefix_end/suffix_start are snapped to char boundaries above the slices"
)]
fn project_evidence_string(
    text: &str,
    report_pointer: &str,
    omissions: &mut Vec<RunReviewEvidenceOmission>,
) -> Value {
    let edge = MAX_PROJECTED_STRING_BYTES / 2;
    let mut prefix_end = edge.min(text.len());
    while !text.is_char_boundary(prefix_end) {
        prefix_end = prefix_end.saturating_sub(1);
    }
    let mut suffix_start = text.len().saturating_sub(edge);
    while !text.is_char_boundary(suffix_start) {
        suffix_start += 1;
    }
    let omitted = &text.as_bytes()[prefix_end..suffix_start];
    let omission = RunReviewEvidenceOmission {
        report_pointer: report_pointer.to_string(),
        kind: "string_bytes".to_string(),
        original_units: text.len(),
        included_units: prefix_end + text.len().saturating_sub(suffix_start),
        omitted_units: omitted.len(),
        omitted_hash: sha256_prefixed(omitted),
    };
    omissions.push(omission.clone());
    serde_json::json!({
        "_harn_review_projection": omission,
        "prefix": &text[..prefix_end],
        "suffix": &text[suffix_start..],
    })
}

fn child_pointer(parent: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{escaped}")
}

fn build_system_prompt() -> String {
    "You review one Harn run report through a deterministic, provenance-bound evidence projection. Use only the supplied JSON evidence. Return only JSON matching the requested schema. Every finding must cite one or more exact RFC 6901 JSON Pointers into the run report evidence document, whose paths mirror the original report. For bounded arrays, cite the report_pointer attached to an included item; never cite projection-only fields. Do not infer omitted evidence, missing private prompts, reasoning, host presentation, or unreported events. Treat projection omissions and checks that mark evidence unavailable, incomplete, or truncated as limits on confidence, not as evidence of failure.".to_string()
}

fn build_prompt(
    evidence: &Value,
    projection: &RunReviewEvidenceProjection,
    rubric: &str,
) -> String {
    // The evidence is serialized as its own top-level document rather than
    // nested beside the projection receipt. A model cites the document it is
    // shown, so a wrapper key would invite pointers rooted at that key instead
    // of at the report. `normalize_pointer` still repairs the wrapped form, but
    // the prompt should not be the thing creating it.
    let projection_json = crate::canonical_json::to_string(&serde_json::json!(projection));
    let evidence_json = crate::canonical_json::to_string(evidence);
    format!(
        "Rubric:\n{rubric}\n\nProjection receipt (how the evidence below was bounded; not citable):\n{projection_json}\n\nRun report evidence (the only evidence; JSON Pointers are rooted at this document):\n{evidence_json}\n\nReturn a concise verdict, findings, and actions. Cite exact JSON Pointers into the run report evidence document."
    )
}

/// Validate the model's review and rewrite its citations into canonical
/// report-rooted JSON Pointers.
fn normalize_model_review(review: &mut ModelRunReview, report: &Value) -> Result<(), String> {
    if !review.confidence.is_finite() || !(0.0..=1.0).contains(&review.confidence) {
        return Err("run review confidence must be between 0 and 1".to_string());
    }
    if review.summary.trim().is_empty() {
        return Err("run review summary must not be empty".to_string());
    }
    for (index, finding) in review.findings.iter_mut().enumerate() {
        if finding.evidence_pointers.is_empty() {
            return Err(format!(
                "run review finding {index} has no evidence pointers"
            ));
        }
        normalize_pointers(
            report,
            &mut finding.evidence_pointers,
            &format!("finding {index}"),
        )?;
    }
    for (index, action) in review.actions.iter_mut().enumerate() {
        normalize_pointers(
            report,
            &mut action.evidence_pointers,
            &format!("action {index}"),
        )?;
    }
    Ok(())
}

/// Wrapper key an earlier prompt shape nested the evidence under. Models that
/// saw it cite `/evidence/...`; the report itself has no such member, so the
/// prefix is stripped when—and only when—the remainder resolves in the report.
const LEGACY_EVIDENCE_POINTER_PREFIX: &str = "/evidence";

/// Rewrite each citation to its canonical report-rooted form, rejecting any
/// pointer that resolves under no accepted rooting. Normalizing here—at the one
/// boundary that owns the model response shape—keeps every stored review
/// citable against the full report while leaving hallucinated pointers
/// fail-closed.
fn normalize_pointers(report: &Value, pointers: &mut [String], owner: &str) -> Result<(), String> {
    for pointer in pointers.iter_mut() {
        match normalize_pointer(report, pointer) {
            Some(canonical) => *pointer = canonical,
            None => {
                return Err(format!(
                    "run review {owner} cites invalid report JSON Pointer {pointer:?}"
                ))
            }
        }
    }
    Ok(())
}

fn normalize_pointer(report: &Value, pointer: &str) -> Option<String> {
    if pointer.is_empty() || !pointer.starts_with('/') {
        return None;
    }
    if report.pointer(pointer).is_some() {
        return Some(pointer.to_string());
    }
    let stripped = pointer.strip_prefix(LEGACY_EVIDENCE_POINTER_PREFIX)?;
    // Only a whole path segment counts: `/evidenced/0` must not become `d/0`.
    // An exact `/evidence` leaves `""`, the RFC 6901 whole-document pointer.
    if !stripped.is_empty() && !stripped.starts_with('/') {
        return None;
    }
    report
        .pointer(stripped)
        .is_some()
        .then(|| stripped.to_string())
}

fn report_limitations(report: &RunReport) -> Vec<RunReviewLimitation> {
    report
        .checks
        .iter()
        .enumerate()
        .filter(|(_, check)| {
            check.status == "unavailable"
                || check.code.contains("coverage")
                || check.code.contains("truncat")
                || check.code.contains("incomplete")
        })
        .map(|(index, check)| RunReviewLimitation {
            code: check.code.clone(),
            message: check.message.clone(),
            evidence_pointer: format!("/checks/{index}"),
        })
        .collect()
}

fn projection_limitations(projection: &RunReviewEvidenceProjection) -> Vec<RunReviewLimitation> {
    projection
        .omissions
        .iter()
        .map(|omission| RunReviewLimitation {
            code: "review_evidence_omitted".to_string(),
            message: format!(
                "bounded review projection retained {} of {} {}; omitted {} with hash {}",
                omission.included_units,
                omission.original_units,
                omission.kind,
                omission.omitted_units,
                omission.omitted_hash
            ),
            evidence_pointer: omission.report_pointer.clone(),
        })
        .collect()
}

fn usage_from_response(response: &crate::value::DictMap, duration_ms: u64) -> RunReviewUsage {
    let usage = response.get("usage").and_then(VmValue::as_dict);
    RunReviewUsage {
        duration_ms,
        input_tokens: usage
            .and_then(|value| value.get("input_tokens"))
            .and_then(VmValue::as_int)
            .unwrap_or_default(),
        output_tokens: usage
            .and_then(|value| value.get("output_tokens"))
            .and_then(VmValue::as_int)
            .unwrap_or_default(),
        cache_read_tokens: usage
            .and_then(|value| value.get("cache_read_tokens"))
            .and_then(VmValue::as_int)
            .unwrap_or_default(),
        cache_write_tokens: usage
            .and_then(|value| value.get("cache_write_tokens"))
            .and_then(VmValue::as_int)
            .unwrap_or_default(),
        cache_hit_ratio: usage
            .and_then(|value| value.get("cache_hit_ratio"))
            .and_then(vm_number),
        cache_visibility: usage
            .and_then(|value| value.get("cache_visibility"))
            .and_then(vm_optional_string),
        cost_usd: usage
            .and_then(|value| value.get("cost_usd"))
            .and_then(vm_number),
    }
}

fn vm_number(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(number) => Some(*number),
        VmValue::Int(number) => Some(*number as f64),
        _ => None,
    }
}

fn vm_optional_string(value: &VmValue) -> Option<String> {
    (!matches!(value, VmValue::Nil)).then(|| value.display())
}

fn idempotency_key(report_hash: &str, rubric_hash: &str, route: &RunReviewModelRoute) -> String {
    let value = serde_json::json!({
        "report_hash": report_hash,
        "rubric_hash": rubric_hash,
        "provider": route.provider,
        "model": route.model,
    });
    sha256_prefixed(&crate::canonical_json::to_vec(&value))
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

struct LifecycleRecorder<'a> {
    clock: &'a dyn Clock,
    receipts: Vec<RunReviewLifecycleReceipt>,
}

impl<'a> LifecycleRecorder<'a> {
    fn new(clock: &'a dyn Clock) -> Self {
        Self {
            clock,
            receipts: Vec::new(),
        }
    }

    fn advance(&mut self, state: RunReviewState) {
        self.receipts.push(RunReviewLifecycleReceipt {
            state,
            at_ms: harn_clock::now_wall_ms(self.clock).max(0) as u64,
        });
    }

    fn invalid(&self, message: String) -> RunReviewError {
        self.terminal(RunReviewState::Invalid, message)
    }

    fn failed(&self, message: String) -> RunReviewError {
        self.terminal(RunReviewState::Failed, message)
    }

    fn finish(mut self, state: RunReviewState) -> RunReviewLifecycle {
        self.advance(state);
        RunReviewLifecycle {
            state,
            receipts: self.receipts,
        }
    }

    fn terminal(&self, state: RunReviewState, message: String) -> RunReviewError {
        let mut receipts = self.receipts.clone();
        receipts.push(RunReviewLifecycleReceipt {
            state,
            at_ms: harn_clock::now_wall_ms(self.clock).max(0) as u64,
        });
        RunReviewError {
            schema: "harn.run_review_error.v1".to_string(),
            message,
            lifecycle: RunReviewLifecycle { state, receipts },
        }
    }
}

fn vm_error_message(error: VmError) -> String {
    format!("run review model call failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{
        run_report_projection_hash, RunReportCheck, RunReportProjection, RUN_REPORT_SCHEMA,
        RUN_REPORT_SCHEMA_VERSION,
    };

    fn report_file(checks: Vec<RunReportCheck>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run-report.json");
        let mut report = RunReport {
            schema: RUN_REPORT_SCHEMA.to_string(),
            schema_version: RUN_REPORT_SCHEMA_VERSION,
            projection: RunReportProjection {
                id: "run_report:root".to_string(),
                hash: String::new(),
            },
            root_run_id: "root".to_string(),
            checks,
            ..RunReport::default()
        };
        report.projection.hash = run_report_projection_hash(&report).expect("report hash");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&report).expect("report JSON"),
        )
        .expect("write report");
        (dir, path)
    }

    fn install_mock(text: &str) {
        let fixture = crate::llm::parse_llm_mocks_jsonl(
            &serde_json::json!({
                "text": text,
                "provider": "openai",
                "model": "gpt-5.6-luna",
                "input_tokens": 120,
                "output_tokens": 40,
                "cache_read_tokens": 20,
                "cache_write_tokens": 10
            })
            .to_string(),
        )
        .expect("mock fixture");
        crate::llm::install_cli_llm_mock_fixture(fixture);
    }

    fn request(path: PathBuf) -> RunReviewRequest {
        RunReviewRequest {
            input: RunReviewInput::Report {
                path,
                allowed_roots: Vec::new(),
            },
            rubric: "Judge coordination and evidence coverage.".to_string(),
            model: Some("gpt-5.6-luna".to_string()),
        }
    }

    #[test]
    fn default_options_select_one_small_tier_call() {
        let value = vm_value_to_json(&build_llm_options(None));
        assert_eq!(value["provider"], "auto");
        assert_eq!(value["model_tier"], "small");
        assert_eq!(value["schema_retries"], 0);
        assert!(value.get("model").is_none());
        assert!(
            value.get("temperature").is_none(),
            "generic review options must leave optional sampling controls to model defaults"
        );
    }

    #[test]
    fn large_import_evidence_is_bounded_with_explicit_omission_provenance() {
        let import_nodes = (0..1_024)
            .map(|index| {
                serde_json::json!({
                    "path": format!("lib/generated/import_{index:04}.rb"),
                    "symbol": format!("GeneratedImport{index:04}"),
                    "detail": "deterministic low-value import edge ".repeat(6),
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "schema": "harn.run_report.v1",
            "projection": {"hash": "sha256:source"},
            "agents": [{"agent_id": "run:root", "status": "completed"}],
            "delegations": [{"parent_agent_id": "run:root", "child_agent_id": "run:child"}],
            "llm_calls": [{"agent_id": "run:root", "input_tokens": 120}],
            "coordination": {"spawned": 1, "terminal": 1, "open": 0},
            "checks": [{"code": "timeline_coverage_incomplete", "status": "unavailable"}],
            "timelines": [{
                "nodes": [{
                    "category": "tool",
                    "kind": "tool_result",
                    "name": "look",
                    "status": "completed",
                    "attributes": {
                        "nodes": import_nodes,
                        "returned": 1_024,
                        "total": 1_934,
                        "truncated": true,
                    },
                }],
            }],
        });
        let source_bytes = crate::canonical_json::to_vec(&report).len();
        assert!(
            source_bytes > 192 * 1_024,
            "fixture must model the observed report class"
        );

        let (evidence, projection) = build_evidence_projection(&report, source_bytes);
        let prompt = build_prompt(&evidence, &projection, "Judge coordination and coverage.");
        let estimated_tokens = crate::llm::estimate_text_tokens(&prompt)
            + crate::llm::estimate_text_tokens(&build_system_prompt());

        assert!(estimated_tokens < MAX_RUN_REVIEW_INPUT_TOKENS);
        assert!(projection.projected_bytes < source_bytes / 4);
        assert_eq!(projection.source_bytes, source_bytes);
        let omission = projection
            .omissions
            .iter()
            .find(|omission| omission.report_pointer == "/timelines/0/nodes/0/attributes/nodes")
            .expect("import-array omission");
        assert_eq!(omission.original_units, 1_024);
        assert_eq!(omission.included_units, MAX_PROJECTED_ARRAY_ITEMS);
        assert_eq!(omission.omitted_units, 992);
        assert!(omission.omitted_hash.starts_with("sha256:"));
        let limitation = projection_limitations(&projection)
            .into_iter()
            .find(|limitation| {
                limitation.evidence_pointer == "/timelines/0/nodes/0/attributes/nodes"
            })
            .expect("projection limitation");
        assert!(limitation.message.contains("omitted 992"));
        assert_eq!(evidence["agents"][0]["agent_id"], "run:root");
        assert_eq!(evidence["coordination"]["terminal"], 1);
        assert_eq!(
            evidence["timelines"][0]["nodes"][0]["attributes"]["nodes"]["items"][0]
                ["report_pointer"],
            "/timelines/0/nodes/0/attributes/nodes/0"
        );
        let (_, repeated) = build_evidence_projection(&report, source_bytes);
        assert_eq!(projection.hash, repeated.hash);
        assert_eq!(projection.omissions, repeated.omissions);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn review_binds_provenance_validates_pointers_and_propagates_coverage() {
        let _env = crate::llm::env_guard();
        crate::llm::clear_cli_llm_mock_mode();
        let (_dir, path) = report_file(vec![RunReportCheck {
            code: "timeline_coverage_incomplete".to_string(),
            severity: "info".to_string(),
            status: "unavailable".to_string(),
            agent_id: Some("run:root".to_string()),
            message: "the timeline omitted events after its bounded query".to_string(),
        }]);
        install_mock(
            r#"{"verdict":"concerns","confidence":0.75,"summary":"The run is structurally sound but timeline coverage is limited.","findings":[{"severity":"warning","title":"Timeline evidence is incomplete","detail":"The report explicitly marks timeline coverage unavailable.","evidence_pointers":["/checks/0"]}],"actions":[{"priority":"next","action":"Inspect the continuation before drawing timing conclusions.","evidence_pointers":["/checks/0"]}]}"#,
        );

        let review = review_run_report(request(path)).await.expect("review");
        crate::llm::clear_cli_llm_mock_mode();

        assert_eq!(review.schema, RUN_REVIEW_SCHEMA);
        assert_eq!(review.lifecycle.state, RunReviewState::Reviewed);
        assert_eq!(
            review
                .lifecycle
                .receipts
                .iter()
                .map(|receipt| receipt.state)
                .collect::<Vec<_>>(),
            vec![
                RunReviewState::Located,
                RunReviewState::Projected,
                RunReviewState::Validated,
                RunReviewState::Reviewing,
                RunReviewState::Reviewed,
            ]
        );
        assert!(review.provenance.report_hash.starts_with("sha256:"));
        assert!(review.provenance.rubric_hash.starts_with("sha256:"));
        assert_eq!(review.provenance.model_route.provider, "openai");
        assert_eq!(review.provenance.model_route.model, "gpt-5.6-luna");
        assert_eq!(
            review.provenance.evidence_projection.schema,
            RUN_REVIEW_EVIDENCE_SCHEMA
        );
        assert!(review
            .provenance
            .evidence_projection
            .hash
            .starts_with("sha256:"));
        assert!(review.provenance.evidence_projection.omissions.is_empty());
        assert!(review.idempotency_key.starts_with("sha256:"));
        assert_eq!(review.findings[0].evidence_pointers, ["/checks/0"]);
        assert_eq!(review.limitations[0].code, "timeline_coverage_incomplete");
        assert_eq!(review.limitations[0].evidence_pointer, "/checks/0");
        assert_eq!(review.usage.input_tokens, 120);
        assert_eq!(review.usage.cache_read_tokens, 20);
        assert!(review.usage.cost_usd.is_some_and(|cost| cost > 0.0));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn run_record_input_has_identical_review_provenance_to_materialized_report() {
        let _env = crate::llm::env_guard();
        crate::llm::clear_cli_llm_mock_mode();
        let dir = tempfile::tempdir().expect("tempdir");
        let run_path = dir.path().join("root.json");
        let report_path = dir.path().join("report.json");
        let run = crate::orchestration::RunRecord {
            type_name: "workflow_run".to_string(),
            id: "root".to_string(),
            workflow_id: "workflow".to_string(),
            status: "completed".to_string(),
            root_run_id: Some("root".to_string()),
            ..crate::orchestration::RunRecord::default()
        };
        crate::orchestration::save_run_record(&run, Some(run_path.to_str().expect("UTF-8 path")))
            .expect("save run");
        let report_request = RunReportRequest {
            run_record_path: run_path.clone(),
            source_root: run_path.parent().map(std::path::Path::to_path_buf),
            ..RunReportRequest::default()
        };
        let report = build_run_report(report_request.clone())
            .await
            .expect("build report");
        std::fs::write(
            &report_path,
            serde_json::to_vec_pretty(&report).expect("report JSON"),
        )
        .expect("write report");
        let response = r#"{"verdict":"pass","confidence":0.9,"summary":"The evidence is sufficient.","findings":[],"actions":[]}"#;

        install_mock(response);
        let from_report = review_run_report(request(report_path))
            .await
            .expect("review report");
        crate::llm::clear_cli_llm_mock_mode();

        install_mock(response);
        let from_run = review_run_report(RunReviewRequest {
            input: RunReviewInput::RunRecord(report_request),
            rubric: "Judge coordination and evidence coverage.".to_string(),
            model: Some("gpt-5.6-luna".to_string()),
        })
        .await
        .expect("review run record");
        crate::llm::clear_cli_llm_mock_mode();

        assert_eq!(from_run.provenance, from_report.provenance);
        assert_eq!(from_run.idempotency_key, from_report.idempotency_key);
        assert_eq!(from_run.findings, from_report.findings);
        assert_eq!(from_run.limitations, from_report.limitations);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn review_fails_closed_on_unknown_evidence_pointer() {
        let _env = crate::llm::env_guard();
        crate::llm::clear_cli_llm_mock_mode();
        let (_dir, path) = report_file(Vec::new());
        install_mock(
            r#"{"verdict":"pass","confidence":0.9,"summary":"Looks good.","findings":[{"severity":"info","title":"Unsupported","detail":"This is not in the report.","evidence_pointers":["/missing"]}],"actions":[]}"#,
        );

        let error = review_run_report(request(path))
            .await
            .expect_err("invalid pointer must fail");
        crate::llm::clear_cli_llm_mock_mode();
        assert_eq!(error.lifecycle.state, RunReviewState::Failed);
        assert!(error.message.contains("invalid report JSON Pointer"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn review_normalizes_projection_rooted_pointers_to_the_report() {
        let _env = crate::llm::env_guard();
        crate::llm::clear_cli_llm_mock_mode();
        let (_dir, path) = report_file(vec![RunReportCheck {
            code: "timeline_coverage_incomplete".to_string(),
            severity: "info".to_string(),
            status: "unavailable".to_string(),
            agent_id: Some("run:root".to_string()),
            message: "the timeline omitted events after its bounded query".to_string(),
        }]);
        // A model that cites the evidence document it was shown, rather than the
        // report, must still produce a usable review: the citation resolves, so
        // it is rewritten to its canonical report-rooted form.
        install_mock(
            r#"{"verdict":"concerns","confidence":0.6,"summary":"Coverage is limited.","findings":[{"severity":"warning","title":"Timeline evidence is incomplete","detail":"Coverage is marked unavailable.","evidence_pointers":["/evidence/checks/0"]}],"actions":[{"priority":"next","action":"Inspect the continuation.","evidence_pointers":["/evidence/root_run_id"]}]}"#,
        );

        let review = review_run_report(request(path)).await.expect("review");
        crate::llm::clear_cli_llm_mock_mode();

        assert_eq!(review.findings[0].evidence_pointers, ["/checks/0"]);
        assert_eq!(review.actions[0].evidence_pointers, ["/root_run_id"]);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn review_fails_closed_on_projection_rooted_pointer_that_resolves_nowhere() {
        let _env = crate::llm::env_guard();
        crate::llm::clear_cli_llm_mock_mode();
        let (_dir, path) = report_file(Vec::new());
        // Stripping the wrapper must not become a way to launder a hallucinated
        // citation: `/missing` is absent from the report either way.
        install_mock(
            r#"{"verdict":"pass","confidence":0.9,"summary":"Looks good.","findings":[{"severity":"info","title":"Unsupported","detail":"Not in the report.","evidence_pointers":["/evidence/missing"]}],"actions":[]}"#,
        );

        let error = review_run_report(request(path))
            .await
            .expect_err("unresolvable pointer must fail");
        crate::llm::clear_cli_llm_mock_mode();
        assert_eq!(error.lifecycle.state, RunReviewState::Failed);
        assert!(error.message.contains("invalid report JSON Pointer"));
    }

    #[test]
    fn pointer_normalization_only_strips_a_whole_wrapper_segment() {
        let report = serde_json::json!({
            "checks": [{"code": "example"}],
            "evidenced": ["not the wrapper"],
        });
        assert_eq!(
            normalize_pointer(&report, "/checks/0/code").as_deref(),
            Some("/checks/0/code")
        );
        assert_eq!(
            normalize_pointer(&report, "/evidence/checks/0/code").as_deref(),
            Some("/checks/0/code")
        );
        // `/evidenced/0` resolves in the report as written and must be left alone.
        assert_eq!(
            normalize_pointer(&report, "/evidenced/0").as_deref(),
            Some("/evidenced/0")
        );
        assert_eq!(normalize_pointer(&report, "/evidence/nope"), None);
        assert_eq!(normalize_pointer(&report, "checks/0"), None);
        assert_eq!(normalize_pointer(&report, ""), None);
    }

    #[test]
    fn prompt_presents_evidence_as_its_own_rooted_document() {
        let evidence = serde_json::json!({"checks": [{"code": "example"}]});
        let projection = RunReviewEvidenceProjection {
            schema: RUN_REVIEW_EVIDENCE_SCHEMA.to_string(),
            hash: "sha256:abc".to_string(),
            source_bytes: 10,
            projected_bytes: 10,
            omissions: Vec::new(),
        };
        let prompt = build_prompt(&evidence, &projection, "Judge it.");
        // The evidence must not be nested under a key that reads as a pointer
        // root; that nesting is what produced `/evidence/...` citations.
        assert!(!prompt.contains(r#""evidence":{"#));
        assert!(prompt.contains("JSON Pointers are rooted at this document"));
        assert!(prompt.contains(r#"{"checks":[{"code":"example"}]}"#));
    }

    #[tokio::test]
    async fn review_rejects_wrong_report_schema_before_model_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-a-report.json");
        std::fs::write(&path, r#"{"schema":"other.v1"}"#).expect("fixture");

        let error = review_run_report(request(path))
            .await
            .expect_err("wrong schema must fail");
        assert_eq!(error.lifecycle.state, RunReviewState::Invalid);
        assert!(error.message.contains("expected harn.run_report.v1"));
        assert!(!error
            .lifecycle
            .receipts
            .iter()
            .any(|receipt| receipt.state == RunReviewState::Reviewing));
    }

    #[tokio::test]
    async fn review_rejects_tampered_report_hash_before_model_call() {
        let (_dir, path) = report_file(Vec::new());
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("report")).expect("JSON");
        value["root_run_id"] = Value::String("tampered".to_string());
        std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON"))
            .expect("tamper report");

        let error = review_run_report(request(path))
            .await
            .expect_err("tampered hash must fail");
        assert_eq!(error.lifecycle.state, RunReviewState::Invalid);
        assert!(error.message.contains("projection hash mismatch"));
        assert!(!error
            .lifecycle
            .receipts
            .iter()
            .any(|receipt| receipt.state == RunReviewState::Reviewing));
    }

    #[tokio::test]
    async fn review_rejects_oversized_input_after_validation_before_model_call() {
        let (_dir, path) = report_file(Vec::new());
        let mut oversized = request(path);
        oversized.rubric = "x".repeat(220_000);

        let error = review_run_report(oversized)
            .await
            .expect_err("oversized review must fail");
        assert_eq!(error.lifecycle.state, RunReviewState::Invalid);
        assert!(error.message.contains("48000-token limit"));
        assert!(error
            .lifecycle
            .receipts
            .iter()
            .any(|receipt| receipt.state == RunReviewState::Validated));
        assert!(!error
            .lifecycle
            .receipts
            .iter()
            .any(|receipt| receipt.state == RunReviewState::Reviewing));
    }
}
