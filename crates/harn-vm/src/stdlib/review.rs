use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::event_log::{active_event_log, install_memory_for_current_thread, AnyEventLog};
use crate::llm::{execute_llm_call, extract_llm_options, vm_value_to_json};
use crate::stdlib::secret_scan::{audit_secret_scan_active, scan_content, SecretFinding};
use crate::triggers::dispatcher::current_dispatch_context;
use crate::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_MAX_ROUNDS: usize = 1;
const MAX_MAX_ROUNDS: usize = 5;
const REVIEW_EVENT_LOG_QUEUE_DEPTH: usize = 128;
const REVIEW_ACTION: &str = "pr.self_review";
const DEFAULT_MODEL_TIER: &str = "small";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: String,
    pub category: String,
    pub title: String,
    pub detail: String,
    pub suggestion: Option<String>,
    pub file: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRound {
    pub round: i64,
    pub summary: String,
    pub findings: Vec<ReviewFinding>,
    pub has_blocking_findings: bool,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewResult {
    pub rubric: String,
    pub rubric_preset: Option<String>,
    pub max_rounds: i64,
    pub summary: String,
    pub findings: Vec<ReviewFinding>,
    pub has_blocking_findings: bool,
    pub rounds: Vec<ReviewRound>,
    pub secret_scan_findings: Vec<SecretFinding>,
    pub trust_record: Option<TrustRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewRoundPayload {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    findings: Vec<ReviewFindingPayload>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewFindingPayload {
    #[serde(default)]
    severity: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    title: String,
    #[serde(default, alias = "description", alias = "body", alias = "message")]
    detail: String,
    #[serde(default)]
    suggestion: Option<String>,
    #[serde(default, alias = "path")]
    file: Option<String>,
    #[serde(default, alias = "line")]
    line_start: Option<i64>,
    #[serde(default)]
    line_end: Option<i64>,
}

struct ReviewTrustInput<'a> {
    diff: &'a str,
    rubric_text: &'a str,
    rubric_preset: Option<&'a str>,
    completed_rounds: usize,
    max_rounds: usize,
    findings: &'a [ReviewFinding],
    secret_scan_findings: &'a [SecretFinding],
    summary: &'a str,
}

pub(crate) fn register_review_builtins(vm: &mut Vm) {
    vm.register_async_builtin(
        "self_review",
        |args| async move { self_review_impl(args).await },
    );
}

async fn self_review_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let diff = args
        .first()
        .map(VmValue::display)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| VmError::Runtime("self_review: diff text is required".to_string()))?;
    let (rubric_text, rubric_preset) = resolve_rubric(args.get(1));
    let max_rounds = resolve_max_rounds(args.get(2))?;

    let event_log = ensure_review_event_log();
    let secret_scan_findings = scan_content(&diff);
    audit_secret_scan_active("stdlib.self_review", diff.len(), &secret_scan_findings).await;
    let secret_review_findings = review_findings_from_secret_scan(&secret_scan_findings);

    let mut rounds = Vec::new();
    let mut final_findings = secret_review_findings.clone();
    let mut final_summary = if secret_scan_findings.is_empty() {
        "No blocking findings.".to_string()
    } else {
        format!(
            "Secret scan found {} blocking finding(s).",
            secret_scan_findings.len()
        )
    };
    let mut previous_findings: Vec<ReviewFinding> = Vec::new();

    for round_index in 0..max_rounds {
        let prompt = build_review_prompt(
            &diff,
            &rubric_text,
            &rubric_preset,
            &secret_scan_findings,
            round_index + 1,
            max_rounds,
            &previous_findings,
        );
        let system = build_review_system_prompt();
        let options = build_review_llm_options();
        let options_dict = options
            .as_dict()
            .cloned()
            .ok_or_else(|| VmError::Runtime("self_review: invalid llm options".to_string()))?;
        let extracted = extract_llm_options(&[
            VmValue::String(Rc::from(prompt.as_str())),
            VmValue::String(Rc::from(system.as_str())),
            options.clone(),
        ])?;
        let response = execute_llm_call(extracted, Some(options_dict), None).await?;
        let response_dict = response.as_dict().ok_or_else(|| {
            VmError::Runtime("self_review: expected llm response dict".to_string())
        })?;
        let data = response_dict.get("data").ok_or_else(|| {
            VmError::Runtime("self_review: llm response missing structured data".to_string())
        })?;
        let round_payload = serde_json::from_value::<ReviewRoundPayload>(vm_value_to_json(data))
            .map_err(|error| VmError::Runtime(format!("self_review: {error}")))?;
        let llm_findings = normalize_llm_findings(round_payload.findings);
        let merged_findings = dedupe_findings(
            llm_findings
                .iter()
                .cloned()
                .chain(secret_review_findings.iter().cloned())
                .collect(),
        );
        let round = ReviewRound {
            round: (round_index + 1) as i64,
            summary: clean_summary(
                round_payload.summary,
                merged_findings.is_empty(),
                secret_scan_findings.len(),
            ),
            has_blocking_findings: has_blocking_findings(&merged_findings),
            findings: merged_findings.clone(),
            model: response_dict.get("model").map(VmValue::display),
            provider: response_dict.get("provider").map(VmValue::display),
            input_tokens: response_dict.get("input_tokens").and_then(VmValue::as_int),
            output_tokens: response_dict.get("output_tokens").and_then(VmValue::as_int),
        };
        let is_stable = !previous_findings.is_empty() && previous_findings == merged_findings;
        previous_findings = merged_findings.clone();
        final_summary = round.summary.clone();
        final_findings = merged_findings;
        rounds.push(round);
        if is_stable {
            break;
        }
    }

    if rounds.is_empty() {
        rounds.push(ReviewRound {
            round: 1,
            summary: final_summary.clone(),
            findings: final_findings.clone(),
            has_blocking_findings: has_blocking_findings(&final_findings),
            model: None,
            provider: None,
            input_tokens: None,
            output_tokens: None,
        });
    }

    let trust_record = append_review_trust_record(
        &event_log,
        ReviewTrustInput {
            diff: &diff,
            rubric_text: &rubric_text,
            rubric_preset: rubric_preset.as_deref(),
            completed_rounds: rounds.len(),
            max_rounds,
            findings: &final_findings,
            secret_scan_findings: &secret_scan_findings,
            summary: &final_summary,
        },
    )
    .await?;

    let result = ReviewResult {
        rubric: rubric_text,
        rubric_preset,
        max_rounds: max_rounds as i64,
        summary: final_summary,
        findings: final_findings.clone(),
        has_blocking_findings: has_blocking_findings(&final_findings),
        rounds,
        secret_scan_findings,
        trust_record: Some(trust_record),
    };
    let value = serde_json::to_value(result)
        .map_err(|error| VmError::Runtime(format!("self_review: {error}")))?;
    Ok(crate::stdlib::json_to_vm_value(&value))
}

fn resolve_rubric(value: Option<&VmValue>) -> (String, Option<String>) {
    let raw = value
        .map(VmValue::display)
        .unwrap_or_else(|| "default".to_string());
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return (
            rubric_preset_body("default").to_string(),
            Some("default".to_string()),
        );
    }
    if let Some(preset) = rubric_preset_name(trimmed) {
        return (
            rubric_preset_body(preset).to_string(),
            Some(preset.to_string()),
        );
    }
    (trimmed.to_string(), None)
}

fn resolve_max_rounds(value: Option<&VmValue>) -> Result<usize, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(DEFAULT_MAX_ROUNDS),
        Some(VmValue::Int(number)) if *number > 0 => Ok((*number as usize).min(MAX_MAX_ROUNDS)),
        Some(VmValue::Int(_)) => Err(VmError::Runtime(
            "self_review: max_rounds must be greater than 0".to_string(),
        )),
        Some(other) => Err(VmError::Runtime(format!(
            "self_review: expected integer max_rounds, got {}",
            other.type_name()
        ))),
    }
}

fn rubric_preset_name(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "default" => Some("default"),
        "code" => Some("code"),
        "docs" => Some("docs"),
        "infra" => Some("infra"),
        "security" => Some("security"),
        _ => None,
    }
}

fn rubric_preset_body(name: &str) -> &'static str {
    match name {
        "code" => {
            "Review for correctness, regressions, missing tests, unsafe assumptions, and API compatibility. Block if the diff is likely wrong or under-tested."
        }
        "docs" => {
            "Review for factual accuracy, drift from implementation, broken examples, missing migration notes, and unclear wording that could mislead users."
        }
        "infra" => {
            "Review for rollout safety, observability, failure modes, config drift, missing rollback notes, and operational regressions."
        }
        "security" => {
            "Review for credential exposure, authz/authn gaps, unsafe data handling, injection risk, and high-signal hardening gaps."
        }
        _ => {
            "Review for correctness, test coverage, security, and style conformance. Prefer high-signal findings only. Block on correctness bugs, missing coverage for risky changes, or credential exposure."
        }
    }
}

fn build_review_system_prompt() -> String {
    r#"You are a strict self-reviewer for a git diff before pull request open. Return only valid JSON in exactly this shape:
{
  "summary": "one concise sentence",
  "findings": [
    {
      "severity": "blocking|warning|info",
      "category": "short category",
      "title": "short title",
      "detail": "specific evidence and impact",
      "suggestion": "optional concrete fix",
      "file": "optional changed file path",
      "line_start": 1,
      "line_end": 1
    }
  ]
}
Use these key names for findings: `severity`, `category`, `title`, `detail`, `suggestion`, `file`, `line_start`, and `line_end`. Omit optional keys or set them to null when unknown.

Prefer no finding over a speculative finding. Report only concrete correctness, security, compatibility, rollout, or test-coverage risks supported by the diff. Do not report style, naming, or preference-only concerns unless they create a concrete defect.

Read every hunk for a changed file before claiming a switch case, branch, config entry, caller, provider, test, or symbol is missing. If the diff adds the allegedly missing name anywhere in the relevant file, drop that finding. A suggestion that only says to verify, ensure, consider, or monitor something is not actionable enough to report."#.to_string()
}

fn build_review_prompt(
    diff: &str,
    rubric_text: &str,
    rubric_preset: &Option<String>,
    secret_findings: &[SecretFinding],
    round: usize,
    max_rounds: usize,
    prior_findings: &[ReviewFinding],
) -> String {
    let rubric_label = rubric_preset.as_deref().unwrap_or("custom");
    if !prior_findings.is_empty() {
        return build_review_adjudication_prompt(
            diff,
            rubric_text,
            rubric_label,
            secret_findings,
            round,
            max_rounds,
            prior_findings,
        );
    }
    let secret_context = if secret_findings.is_empty() {
        "Secret scan: clean.".to_string()
    } else {
        let findings = secret_findings
            .iter()
            .map(|finding| {
                format!(
                    "- {} on line {} ({}) [{}]",
                    finding.title, finding.line, finding.redacted, finding.detector
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("Secret scan findings:\n{findings}")
    };
    format!(
        "Self-review round {round} of {max_rounds}.\n\
Rubric preset: {rubric_label}\n\
Rubric:\n{rubric_text}\n\n\
Rules:\n\
- Only report issues supported by the diff.\n\
- Do not ask the author to merely verify, ensure, or consider something; a finding must name a concrete broken contract or risk.\n\
- A diff is not a full repository listing. Do not claim a required file, provider, config, test, or caller is missing solely because it is absent from the diff.\n\
- Check all hunks in a changed file before reporting a missing switch case, branch, config entry, caller, provider, test, or symbol; the diff may add it in a later hunk.\n\
- Do not preserve prior-round findings by default. Treat them as candidates to disprove; the final round should be cleaner and more evidence-backed than the first.\n\
- Hypothetical compatibility or rollout concerns are not findings unless the diff directly changes a supported contract and explains the likely failure.\n\
- Drop any candidate whose only concrete recommendation is to verify, ensure, consider, or monitor behavior.\n\
- Use severity `blocking` for issues that should stop PR open.\n\
- Use severity `warning` for non-blocking but important follow-up.\n\
- Use severity `info` sparingly.\n\
- Prefer the smallest set of high-signal findings.\n\
- If the diff is clean, return an empty findings list.\n\n\
{secret_context}\n\n\
Diff:\n```diff\n{diff}\n```"
    )
}

fn build_review_adjudication_prompt(
    diff: &str,
    rubric_text: &str,
    rubric_label: &str,
    secret_findings: &[SecretFinding],
    round: usize,
    max_rounds: usize,
    prior_findings: &[ReviewFinding],
) -> String {
    let secret_context = if secret_findings.is_empty() {
        "Secret scan: clean.".to_string()
    } else {
        format!(
            "Secret scan injected {} deterministic finding(s); do not duplicate them.",
            secret_findings.len()
        )
    };
    let candidates =
        serde_json::to_string_pretty(prior_findings).unwrap_or_else(|_| "[]".to_string());
    format!(
        "Self-review adjudication round {round} of {max_rounds}.\n\
Rubric preset: {rubric_label}\n\
Rubric:\n{rubric_text}\n\n\
Your job is to verify candidate findings from the previous round. Do not add new findings in this round. Return only the subset of candidates that are still clearly supported by the diff, optionally tightening their wording, severity, line range, or suggestion.\n\n\
Drop a candidate when any of these are true:\n\
- The candidate points to a file that is not changed in the diff.\n\
- The candidate claims a switch case, branch, config entry, caller, provider, test, or symbol is missing, but the diff adds that name anywhere in the relevant file.\n\
- The candidate relies on absence from the diff as proof of absence from the repository.\n\
- The candidate is hypothetical, preference-only, or only asks the author to verify, ensure, consider, or monitor behavior.\n\
- The candidate cannot be tied to a concrete changed line and likely user-visible failure.\n\n\
{secret_context}\n\n\
Candidate findings JSON:\n```json\n{candidates}\n```\n\n\
Diff:\n```diff\n{diff}\n```"
    )
}

fn build_review_llm_options() -> VmValue {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["summary", "findings"],
        "additionalProperties": false,
        "properties": {
            "summary": {"type": "string"},
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "severity": {"type": "string", "enum": ["blocking", "warning", "info"]},
                        "category": {"type": "string"},
                        "title": {"type": "string"},
                        "detail": {"type": "string"},
                        "description": {"type": "string"},
                        "body": {"type": "string"},
                        "message": {"type": "string"},
                        "suggestion": {"type": ["string", "null"]},
                        "file": {"type": ["string", "null"]},
                        "path": {"type": ["string", "null"]},
                        "line_start": {"type": ["integer", "null"]},
                        "line": {"type": ["integer", "null"]},
                        "line_end": {"type": ["integer", "null"]}
                    }
                }
            }
        }
    });
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "provider": "auto",
        "model_tier": DEFAULT_MODEL_TIER,
        "temperature": 0.1,
        "response_format": "json",
        "output_schema": schema,
        "output_validation": "error",
        "schema_retries": 3,
    }))
}

fn normalize_llm_findings(payloads: Vec<ReviewFindingPayload>) -> Vec<ReviewFinding> {
    let mut findings = Vec::new();
    for payload in payloads {
        let severity = match payload.severity.as_str() {
            "blocking" | "warning" | "info" => payload.severity,
            _ => "warning".to_string(),
        };
        let category = if payload.category.trim().is_empty() {
            "general".to_string()
        } else {
            payload.category.trim().to_string()
        };
        let title = if payload.title.trim().is_empty() {
            "Review finding".to_string()
        } else {
            payload.title.trim().to_string()
        };
        let detail = if payload.detail.trim().is_empty() {
            title.clone()
        } else {
            payload.detail.trim().to_string()
        };
        let file = payload
            .file
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let line_start = payload.line_start.filter(|line| *line > 0);
        let line_end = payload
            .line_end
            .filter(|line| *line > 0)
            .or(line_start)
            .filter(|line| line_start.map(|start| *line >= start).unwrap_or(true));
        let mut finding = ReviewFinding {
            id: String::new(),
            severity,
            category,
            title,
            detail,
            suggestion: payload
                .suggestion
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            file,
            line_start,
            line_end,
            source: "llm".to_string(),
        };
        finding.id = finding_id(&finding);
        findings.push(finding);
    }
    dedupe_findings(findings)
}

fn review_findings_from_secret_scan(findings: &[SecretFinding]) -> Vec<ReviewFinding> {
    findings
        .iter()
        .map(|finding| {
            let mut review = ReviewFinding {
                id: String::new(),
                severity: "blocking".to_string(),
                category: "security".to_string(),
                title: finding.title.clone(),
                detail: format!(
                    "Secret scan detected a candidate credential with detector `{}` at line {}.",
                    finding.detector, finding.line
                ),
                suggestion: Some(
                    "Remove the secret from the diff and rotate it if it is real.".to_string(),
                ),
                file: None,
                line_start: Some(finding.line as i64),
                line_end: Some(finding.line as i64),
                source: "secret_scan".to_string(),
            };
            review.id = format!("secret-{}", finding.fingerprint);
            review
        })
        .collect()
}

fn dedupe_findings(findings: Vec<ReviewFinding>) -> Vec<ReviewFinding> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for finding in findings {
        let key = (
            finding.source.clone(),
            finding.severity.clone(),
            finding.category.clone(),
            finding.title.clone(),
            finding.detail.clone(),
            finding.file.clone(),
            finding.line_start,
            finding.line_end,
        );
        if seen.insert(key) {
            deduped.push(finding);
        }
    }
    deduped
}

fn has_blocking_findings(findings: &[ReviewFinding]) -> bool {
    findings
        .iter()
        .any(|finding| finding.severity == "blocking")
}

fn clean_summary(summary: String, clean: bool, secret_count: usize) -> String {
    let trimmed = summary.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    if secret_count > 0 {
        return format!("Secret scan found {secret_count} blocking finding(s).");
    }
    if clean {
        "No high-signal findings.".to_string()
    } else {
        "Review completed with findings.".to_string()
    }
}

fn finding_id(finding: &ReviewFinding) -> String {
    let seed = format!(
        "{}|{}|{}|{}|{}|{:?}|{:?}|{:?}",
        finding.source,
        finding.severity,
        finding.category,
        finding.title,
        finding.detail,
        finding.file,
        finding.line_start,
        finding.line_end
    );
    let digest = sha2::Sha256::digest(seed.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ensure_review_event_log() -> Arc<AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(REVIEW_EVENT_LOG_QUEUE_DEPTH))
}

async fn append_review_trust_record(
    log: &Arc<AnyEventLog>,
    input: ReviewTrustInput<'_>,
) -> Result<TrustRecord, VmError> {
    let (agent, autonomy_tier, trace_id) = review_identity();
    let outcome = if has_blocking_findings(input.findings) {
        TrustOutcome::Failure
    } else {
        TrustOutcome::Success
    };
    let mut record = TrustRecord::new(agent, REVIEW_ACTION, None, outcome, trace_id, autonomy_tier);
    record
        .metadata
        .insert("rubric".to_string(), serde_json::json!(input.rubric_text));
    record.metadata.insert(
        "rubric_preset".to_string(),
        input
            .rubric_preset
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    record.metadata.insert(
        "requested_rounds".to_string(),
        serde_json::json!(input.max_rounds),
    );
    record.metadata.insert(
        "completed_rounds".to_string(),
        serde_json::json!(input.completed_rounds),
    );
    record.metadata.insert(
        "finding_count".to_string(),
        serde_json::json!(input.findings.len()),
    );
    record.metadata.insert(
        "blocking_finding_count".to_string(),
        serde_json::json!(input
            .findings
            .iter()
            .filter(|finding| finding.severity == "blocking")
            .count()),
    );
    record.metadata.insert(
        "secret_scan_finding_count".to_string(),
        serde_json::json!(input.secret_scan_findings.len()),
    );
    record.metadata.insert(
        "finding_categories".to_string(),
        serde_json::json!(input
            .findings
            .iter()
            .map(|finding| finding.category.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()),
    );
    record
        .metadata
        .insert("summary".to_string(), serde_json::json!(input.summary));
    record.metadata.insert(
        "diff_bytes".to_string(),
        serde_json::json!(input.diff.len()),
    );
    record.metadata.insert(
        "diff_sha256".to_string(),
        serde_json::json!(sha256_hex(input.diff.as_bytes())),
    );
    let record = append_trust_record(log, &record)
        .await
        .map_err(|error| VmError::Runtime(format!("self_review: {error}")))?;
    Ok(record)
}

fn review_identity() -> (String, AutonomyTier, String) {
    if let Some(context) = current_dispatch_context() {
        return (
            context.agent_id,
            context.autonomy_tier,
            context.trigger_event.trace_id.0,
        );
    }
    let username = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    let workspace = crate::stdlib::process::source_root_path()
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "workspace".to_string());
    (
        format!("local::{username}::{workspace}"),
        AutonomyTier::Suggest,
        format!("trace-{}", uuid::Uuid::now_v7()),
    )
}

fn sha256_hex(input: &[u8]) -> String {
    let digest = sha2::Sha256::digest(input);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::compiler::Compiler;
    use crate::llm::helpers::reset_provider_key_cache;
    use crate::stdlib::register_vm_stdlib;
    use crate::value::VmValue;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn run_script(source: &str) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut lexer = Lexer::new(source);
                    let tokens = lexer.tokenize().unwrap();
                    let mut parser = Parser::new(tokens);
                    let program = parser.parse().unwrap();
                    let chunk = Compiler::new().compile(&program).unwrap();

                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    let _ = vm.execute(&chunk).await.unwrap();
                    vm.output().trim_end().to_string()
                })
                .await
        })
    }

    fn with_mock_provider<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        let prev_model = std::env::var("HARN_LLM_MODEL").ok();
        unsafe {
            std::env::set_var("HARN_LLM_PROVIDER", "mock");
            std::env::remove_var("HARN_LLM_MODEL");
        }
        reset_provider_key_cache();
        crate::llm::mock::reset_llm_mock_state();
        crate::event_log::reset_active_event_log();
        crate::stdlib::reset_stdlib_state();
        let result = f();
        crate::llm::mock::reset_llm_mock_state();
        crate::event_log::reset_active_event_log();
        crate::stdlib::reset_stdlib_state();
        unsafe {
            match prev_provider {
                Some(value) => std::env::set_var("HARN_LLM_PROVIDER", value),
                None => std::env::remove_var("HARN_LLM_PROVIDER"),
            }
            match prev_model {
                Some(value) => std::env::set_var("HARN_LLM_MODEL", value),
                None => std::env::remove_var("HARN_LLM_MODEL"),
            }
        }
        reset_provider_key_cache();
        result
    }

    #[test]
    fn resolve_rubric_supports_presets_and_custom_text() {
        let default = resolve_rubric(None);
        assert_eq!(default.1.as_deref(), Some("default"));
        assert!(default.0.contains("test coverage"));

        let code = resolve_rubric(Some(&VmValue::String(Rc::from("code"))));
        assert_eq!(code.1.as_deref(), Some("code"));
        assert!(code.0.contains("regressions"));

        let custom = resolve_rubric(Some(&VmValue::String(Rc::from("focus on docs drift"))));
        assert_eq!(custom.1, None);
        assert_eq!(custom.0, "focus on docs drift");
    }

    #[test]
    fn self_review_system_prompt_names_exact_json_shape() {
        let prompt = build_review_system_prompt();
        assert!(prompt.contains("\"summary\""));
        assert!(prompt.contains("\"findings\""));
        assert!(prompt.contains("\"detail\""));
        assert!(prompt.contains("Use these key names"));
        assert!(prompt.contains("Prefer no finding over a speculative finding"));
        assert!(prompt.contains("Read every hunk"));
    }

    #[test]
    fn self_review_records_llm_and_trust_results() {
        with_mock_provider(|| {
            let out = run_script(
                r#"
import "std/review"
import "std/triggers"

pipeline test(task) {
  llm_mock({
    text: "{\"summary\":\"Parser change needs a regression test.\",\"findings\":[{\"severity\":\"blocking\",\"category\":\"test_coverage\",\"title\":\"Missing regression test\",\"detail\":\"The parser behavior changed without a focused regression test.\",\"suggestion\":\"Add a conformance case.\",\"file\":\"conformance/tests/parser_case.harn\",\"line_start\":1,\"line_end\":1}]}"
  })
  let result: ReviewResult = self_review("diff --git a/src/parser.rs b/src/parser.rs\n+parse_new_branch()", "code", 1)
  println(result.rubric_preset == "code")
  println(result.has_blocking_findings)
  println(result.findings[0].source == "llm")
  let records: list<TrustRecord> = trust_query({action: "pr.self_review"})
  println(len(records) == 1)
  println(records[0].metadata.blocking_finding_count == 1)
}
"#,
            );
            assert_eq!(out, "true\ntrue\ntrue\ntrue\ntrue");
        });
    }

    #[test]
    fn self_review_merges_secret_scan_findings() {
        with_mock_provider(|| {
            let out = run_script(
                r#"
import "std/review"

pipeline test(task) {
  llm_mock({text: "{\"summary\":\"No extra issues.\",\"findings\":[]}"})
  let result: ReviewResult = self_review("diff --git a/.env b/.env\n+OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwx123456", "default", 1)
  println(result.has_blocking_findings)
  println(len(result.secret_scan_findings) == 1)
  println(result.findings[0].source == "secret_scan")
}
"#,
            );
            assert_eq!(out, "true\ntrue\ntrue");
        });
    }

    #[test]
    fn self_review_normalizes_missing_finding_fields() {
        with_mock_provider(|| {
            let out = run_script(
                r#"
import "std/review"

pipeline test(task) {
  llm_mock({
    text: "{\"summary\":\"One issue.\",\"findings\":[{\"severity\":\"warning\",\"category\":\"runtime_safety\",\"detail\":\"The changed path can crash on startup.\"}]}"
  })
  let result: ReviewResult = self_review("diff --git a/src/main.rs b/src/main.rs\n+panic!(\"startup\")", "code", 1)
  println(len(result.findings) == 1)
  println(result.findings[0].title == "Review finding")
  println(result.findings[0].detail == "The changed path can crash on startup.")
}
"#,
            );
            assert_eq!(out, "true\ntrue\ntrue");
        });
    }

    #[test]
    fn self_review_accepts_common_finding_aliases() {
        with_mock_provider(|| {
            let out = run_script(
                r#"
import "std/review"

pipeline test(task) {
  llm_mock({
    text: "{\"summary\":\"One issue.\",\"findings\":[{\"severity\":\"warning\",\"category\":\"runtime_safety\",\"title\":\"Startup crash\",\"description\":\"The changed path can crash on startup.\",\"path\":\"src/main.rs\",\"line\":1}]}"
  })
  let result: ReviewResult = self_review("diff --git a/src/main.rs b/src/main.rs\n+panic!(\"startup\")", "code", 1)
  println(result.findings[0].detail == "The changed path can crash on startup.")
  println(result.findings[0].file == "src/main.rs")
  println(result.findings[0].line_start == 1)
}
"#,
            );
            assert_eq!(out, "true\ntrue\ntrue");
        });
    }

    #[test]
    fn self_review_runs_multiple_rounds_and_includes_prior_findings_in_prompt() {
        with_mock_provider(|| {
            let out = run_script(
                r#"
import "std/review"

pipeline test(task) {
  llm_mock({
    text: "{\"summary\":\"Maybe add a test.\",\"findings\":[{\"severity\":\"warning\",\"category\":\"test_coverage\",\"title\":\"Consider a test\",\"detail\":\"A narrow regression test would help.\"}]}"
  })
  llm_mock({
    text: "{\"summary\":\"Clean after reconsidering the evidence.\",\"findings\":[]}"
  })
  let result: ReviewResult = self_review("diff --git a/src/lib.rs b/src/lib.rs\n+let value = 1", "default", 2)
  let calls = llm_mock_calls()
  let second_prompt = calls[1].messages[0].content
  println(len(result.rounds) == 2)
  println(result.rounds[1].summary == "Clean after reconsidering the evidence.")
  println(contains(second_prompt, "Candidate findings JSON"))
  println(contains(second_prompt, "Do not add new findings"))
}
"#,
            );
            assert_eq!(out, "true\ntrue\ntrue\ntrue");
        });
    }
}
