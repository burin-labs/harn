//! Harn-side bindings for Flow predicate result construction.
//!
//! These builtins let `.harn` predicate authors return idiomatic
//! [`InvariantResult`](crate::flow::InvariantResult) values: graded verdicts
//! (`Allow`, `Warn`, `Block`, `RequireApproval`), structured evidence, and
//! optional remediation suggestions.
//!
//! See issue #581 for the type contract and parent epic #571 for the wider
//! Harn Flow shipping substrate. The Rust types live in
//! `crates/harn-vm/src/flow/predicates/result.rs`; this module is a thin layer
//! that constructs equivalent record values via the VM and exposes a few
//! introspection helpers (`flow_invariant_kind`, `flow_invariant_is_blocking`,
//! `flow_invariant_confidence`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::flow::{
    parse_invariants_source, Approver, ByteSpan, EvidenceItem, InvariantBlockError,
    InvariantResult, PredicateKind, Remediation, Verdict,
};
use crate::llm::helpers::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm, VmBuiltinArity, VmBuiltinMetadata};

const DEFAULT_FEEDBACK_MAX_ITEMS: usize = 8;
const HARD_FEEDBACK_MAX_ITEMS: usize = 50;
const DEFAULT_FEEDBACK_MAX_FINDINGS_PER_ITEM: usize = 3;
const HARD_FEEDBACK_MAX_FINDINGS_PER_ITEM: usize = 20;
const DEFAULT_FEEDBACK_MAX_MESSAGE_CHARS: usize = 240;
const HARD_FEEDBACK_MAX_MESSAGE_CHARS: usize = 2_000;

pub(crate) fn register_flow_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    vm.register_async_builtin_with_metadata(
        VmBuiltinMetadata::async_static("flow_evaluate_invariants")
            .signature_static("flow_evaluate_invariants(source: string, slice: dict, options?: dict) -> dict")
            .arity(VmBuiltinArity::Range { min: 2, max: 3 })
            .category_static("flow")
            .doc_static("Evaluate Flow `@invariant` predicate functions from Harn source or a module path against a slice and return typed execution records."),
        |ctx, args| async move { flow_evaluate_invariants_impl(ctx, args).await },
    );
}

#[harn_builtin(sig = "flow_invariant_allow() -> dict", category = "flow")]
fn flow_invariant_allow_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(InvariantResult::allow().to_vm_value())
}

#[harn_builtin(sig = "flow_invariant_warn(reason: string) -> dict", category = "flow")]
fn flow_invariant_warn_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let reason = required_string(args, 0, "flow_invariant_warn", "reason")?;
    Ok(InvariantResult::warn(reason).to_vm_value())
}

#[harn_builtin(
    sig = "flow_invariant_block(code: string, message: string) -> dict",
    category = "flow"
)]
fn flow_invariant_block_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let code = required_string(args, 0, "flow_invariant_block", "code")?;
    let message = required_string(args, 1, "flow_invariant_block", "message")?;
    Ok(InvariantResult::block(InvariantBlockError::new(code, message)).to_vm_value())
}

#[harn_builtin(
    sig = "flow_invariant_require_approval(kind: string, id: string) -> dict",
    category = "flow"
)]
fn flow_invariant_require_approval_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let kind = required_string(args, 0, "flow_invariant_require_approval", "kind")?;
    let id = required_string(args, 1, "flow_invariant_require_approval", "id")?;
    let approver = match kind.as_str() {
        "principal" => Approver::principal(id),
        "role" => Approver::role(id),
        other => {
            return Err(VmError::Runtime(format!(
                "flow_invariant_require_approval: kind must be \"principal\" or \"role\", got \"{other}\""
            )));
        }
    };
    Ok(InvariantResult::require_approval(approver).to_vm_value())
}

#[harn_builtin(
    sig = "flow_evidence_atom(atom_id: string, diff_start: int, diff_end: int) -> dict",
    category = "flow"
)]
fn flow_evidence_atom_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let atom_hex = required_string(args, 0, "flow_evidence_atom", "atom_id")?;
    let atom = parse_atom_id(&atom_hex, "flow_evidence_atom")?;
    let start = required_u64(args, 1, "flow_evidence_atom", "diff_start")?;
    let end = required_u64(args, 2, "flow_evidence_atom", "diff_end")?;
    validate_span(start, end, "flow_evidence_atom")?;
    Ok(serde_to_vm(&EvidenceItem::AtomPointer {
        atom,
        diff_span: ByteSpan::new(start, end),
    }))
}

#[harn_builtin(
    sig = "flow_evidence_metadata(directory: string, namespace: string, key: string) -> dict",
    category = "flow"
)]
fn flow_evidence_metadata_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let directory = required_string(args, 0, "flow_evidence_metadata", "directory")?;
    let namespace = required_string(args, 1, "flow_evidence_metadata", "namespace")?;
    let key = required_string(args, 2, "flow_evidence_metadata", "key")?;
    Ok(serde_to_vm(&EvidenceItem::MetadataPath {
        directory,
        namespace,
        key,
    }))
}

#[harn_builtin(
    sig = "flow_evidence_transcript(transcript_id: string, span_start: int, span_end: int) -> dict",
    category = "flow"
)]
fn flow_evidence_transcript_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let transcript_id = required_string(args, 0, "flow_evidence_transcript", "transcript_id")?;
    let start = required_u64(args, 1, "flow_evidence_transcript", "span_start")?;
    let end = required_u64(args, 2, "flow_evidence_transcript", "span_end")?;
    validate_span(start, end, "flow_evidence_transcript")?;
    Ok(serde_to_vm(&EvidenceItem::TranscriptExcerpt {
        transcript_id,
        span: ByteSpan::new(start, end),
    }))
}

#[harn_builtin(
    sig = "flow_evidence_citation(url: string, quote: string, fetched_at: string) -> dict",
    category = "flow"
)]
fn flow_evidence_citation_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let url = required_string(args, 0, "flow_evidence_citation", "url")?;
    let quote = required_string(args, 1, "flow_evidence_citation", "quote")?;
    let fetched_at = required_string(args, 2, "flow_evidence_citation", "fetched_at")?;
    Ok(serde_to_vm(&EvidenceItem::ExternalCitation {
        url,
        quote,
        fetched_at,
    }))
}

#[harn_builtin(
    sig = "flow_remediation(description: string) -> dict",
    category = "flow"
)]
fn flow_remediation_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let description = required_string(args, 0, "flow_remediation", "description")?;
    Ok(serde_to_vm(&Remediation::describe(description)))
}

#[harn_builtin(
    sig = "flow_with_evidence(result: dict, evidence: list) -> dict",
    category = "flow"
)]
fn flow_with_evidence_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut result = require_invariant(args, 0, "flow_with_evidence")?;
    let list = require_list_arg(args, 1, "flow_with_evidence", "evidence")?;
    let evidence = list
        .iter()
        .map(decode_evidence_item)
        .collect::<Result<Vec<_>, _>>()?;
    result = result.with_evidence(evidence);
    Ok(result.to_vm_value())
}

#[harn_builtin(
    sig = "flow_with_remediation(result: dict, remediation: dict) -> dict",
    category = "flow"
)]
fn flow_with_remediation_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut result = require_invariant(args, 0, "flow_with_remediation")?;
    let remediation = decode_remediation(args.get(1).unwrap_or(&VmValue::Nil))?;
    result = result.with_remediation(remediation);
    Ok(result.to_vm_value())
}

#[harn_builtin(
    sig = "flow_with_confidence(result: dict, confidence: float | int) -> dict",
    category = "flow"
)]
fn flow_with_confidence_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut result = require_invariant(args, 0, "flow_with_confidence")?;
    let confidence = required_f64(args, 1, "flow_with_confidence", "confidence")?;
    result = result.with_confidence(confidence);
    Ok(result.to_vm_value())
}

#[harn_builtin(sig = "flow_invariant_kind(result: dict) -> string", category = "flow")]
fn flow_invariant_kind_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let result = require_invariant(args, 0, "flow_invariant_kind")?;
    let kind = match &result.verdict {
        Verdict::Allow => "allow",
        Verdict::Warn { .. } => "warn",
        Verdict::Block { .. } => "block",
        Verdict::RequireApproval { .. } => "require_approval",
    };
    Ok(VmValue::String(arcstr::ArcStr::from(kind)))
}

#[harn_builtin(
    sig = "flow_invariant_is_blocking(result: dict) -> bool",
    category = "flow"
)]
fn flow_invariant_is_blocking_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let result = require_invariant(args, 0, "flow_invariant_is_blocking")?;
    Ok(VmValue::Bool(result.is_blocking()))
}

#[harn_builtin(
    sig = "flow_invariant_confidence(result: dict) -> float",
    category = "flow"
)]
fn flow_invariant_confidence_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let result = require_invariant(args, 0, "flow_invariant_confidence")?;
    Ok(VmValue::Float(result.confidence))
}

#[harn_builtin(
    sig = "flow_invariant_feedback(report: dict, options?: dict) -> string",
    category = "flow"
)]
fn flow_invariant_feedback_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let report = args
        .first()
        .ok_or_else(|| VmError::Runtime("flow_invariant_feedback: missing report".to_string()))?;
    if !matches!(report, VmValue::Dict(_)) {
        return Err(VmError::Runtime(format!(
            "flow_invariant_feedback: report must be a dict, got {}",
            report.type_name()
        )));
    }
    let report = value_to_json(report);
    let report = report.as_object().ok_or_else(|| {
        VmError::Runtime("flow_invariant_feedback: report must be a dict".to_string())
    })?;
    let options = FlowFeedbackOptions::parse(args.get(1))?;
    let feedback = build_flow_feedback(report, &options);
    Ok(VmValue::String(arcstr::ArcStr::from(feedback)))
}

async fn flow_evaluate_invariants_impl(
    ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let request = InvariantEvalRequest::parse(&args)?;
    let parsed = parse_invariants_source(&request.source);
    let mut diagnostics = Vec::new();
    for diagnostic in &parsed.diagnostics {
        diagnostics.push(serde_json::json!({
            "severity": match diagnostic.severity {
                crate::flow::DiscoveryDiagnosticSeverity::Warning => "warning",
                crate::flow::DiscoveryDiagnosticSeverity::Error => "error",
            },
            "message": diagnostic.message,
            "span": diagnostic.span.map(|span| serde_json::json!({
                "start": span.start,
                "end": span.end,
            })),
        }));
    }

    if parsed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == crate::flow::DiscoveryDiagnosticSeverity::Error)
    {
        return Ok(json_to_vm_value(&serde_json::json!({
            "ok": false,
            "status": "discovery_error",
            "diagnostics": diagnostics,
            "records": [],
            "skipped": [],
        })));
    }

    let mut vm = ctx.child_vm();
    let exports = match &request.module_path {
        Some(path) => vm.load_module_exports(path).await?,
        None => {
            vm.load_module_exports_from_source(
                request
                    .source_key
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("<flow-evaluate-invariants>.harn")),
                &request.source,
            )
            .await?
        }
    };

    let slice_arg = json_to_vm_value(&request.slice);
    let predicate_ctx_arg = json_to_vm_value(&request.predicate_ctx);
    let repo_at_base_arg = request
        .repo_at_base
        .as_ref()
        .map(json_to_vm_value)
        .unwrap_or(VmValue::Nil);
    let mut records = Vec::new();
    let mut skipped = Vec::new();

    for predicate in parsed.predicates {
        let kind = predicate.kind;
        if kind == PredicateKind::Semantic && !request.include_semantic {
            skipped.push(serde_json::json!({
                "name": predicate.name,
                "hash": predicate.source_hash.as_str(),
                "kind": "semantic",
                "reason": "semantic predicates require an explicit include_semantic option",
            }));
            continue;
        }
        if !request
            .predicate_names
            .as_ref()
            .map(|names| names.iter().any(|name| name == &predicate.name))
            .unwrap_or(true)
        {
            skipped.push(serde_json::json!({
                "name": predicate.name,
                "hash": predicate.source_hash.as_str(),
                "kind": predicate_kind_label(kind),
                "reason": "not selected",
            }));
            continue;
        }

        let Some(closure) = exports.get(&predicate.name).cloned() else {
            records.push(predicate_error_record(
                &predicate.name,
                predicate.source_hash.as_str(),
                kind,
                "predicate_missing_export",
                format!("invariant `{}` was parsed but not exported", predicate.name),
            ));
            continue;
        };

        let first = tokio::time::timeout(
            request.budget,
            vm.call_closure_pub(
                &closure,
                &[
                    slice_arg.clone(),
                    predicate_ctx_arg.clone(),
                    repo_at_base_arg.clone(),
                ],
            ),
        )
        .await;
        let first = match first {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                records.push(predicate_error_record(
                    &predicate.name,
                    predicate.source_hash.as_str(),
                    kind,
                    "predicate_runtime_error",
                    error.to_string(),
                ));
                continue;
            }
            Err(_) => {
                records.push(predicate_error_record(
                    &predicate.name,
                    predicate.source_hash.as_str(),
                    kind,
                    "budget_exceeded",
                    format!(
                        "{:?} predicate exceeded {}ms budget",
                        kind,
                        request.budget.as_millis()
                    ),
                ));
                continue;
            }
        };
        let first_json = vm_value_to_json(&first);
        let result = adapt_invariant_result(&first).unwrap_or_else(|message| {
            InvariantResult::block(InvariantBlockError::new(
                "invalid_predicate_result",
                message,
            ))
        });
        records.push(serde_json::json!({
            "name": predicate.name,
            "hash": predicate.source_hash.as_str(),
            "kind": predicate_kind_label(kind),
            "result": serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
            "raw_result": first_json,
            "attempts": 1,
            "replayable": kind == PredicateKind::Deterministic,
        }));
    }

    let ok = records.iter().all(|record| {
        record
            .get("result")
            .and_then(|result| result.get("verdict"))
            .and_then(|verdict| verdict.get("kind"))
            .and_then(serde_json::Value::as_str)
            != Some("block")
    });
    Ok(json_to_vm_value(&serde_json::json!({
        "ok": ok,
        "status": if ok { "pass" } else { "fail" },
        "diagnostics": diagnostics,
        "records": records,
        "skipped": skipped,
    })))
}

struct InvariantEvalRequest {
    source: String,
    source_key: Option<PathBuf>,
    module_path: Option<PathBuf>,
    slice: serde_json::Value,
    predicate_ctx: serde_json::Value,
    repo_at_base: Option<serde_json::Value>,
    predicate_names: Option<Vec<String>>,
    include_semantic: bool,
    budget: Duration,
}

impl InvariantEvalRequest {
    fn parse(args: &[VmValue]) -> Result<Self, VmError> {
        let options = args
            .get(2)
            .map(vm_value_to_json)
            .unwrap_or_else(|| serde_json::json!({}));
        let options = options.as_object();
        let module_path = options
            .and_then(|map| map.get("path").or_else(|| map.get("module_path")))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let source = if let Some(path) = &module_path {
            std::fs::read_to_string(path).map_err(|error| {
                VmError::Runtime(format!(
                    "flow_evaluate_invariants: failed to read {}: {error}",
                    path.display()
                ))
            })?
        } else {
            required_string(args, 0, "flow_evaluate_invariants", "source")?
        };
        let slice = args.get(1).map(vm_value_to_json).ok_or_else(|| {
            VmError::Runtime(
                "flow_evaluate_invariants: missing required slice argument".to_string(),
            )
        })?;
        let predicate_ctx = options
            .and_then(|map| map.get("ctx").or_else(|| map.get("predicate_ctx")))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let repo_at_base = options
            .and_then(|map| map.get("repo_at_base").cloned())
            .filter(|value| !value.is_null());
        let predicate_names = options
            .and_then(|map| map.get("predicate_names").or_else(|| map.get("predicates")))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|items| !items.is_empty());
        let source_key = options
            .and_then(|map| map.get("source_key"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let include_semantic = options
            .and_then(|map| map.get("include_semantic"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let budget_ms = options
            .and_then(|map| map.get("budget_ms"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(50)
            .max(1);

        Ok(Self {
            source,
            source_key,
            module_path,
            slice,
            predicate_ctx,
            repo_at_base,
            predicate_names,
            include_semantic,
            budget: Duration::from_millis(budget_ms),
        })
    }
}

fn adapt_invariant_result(value: &VmValue) -> Result<InvariantResult, String> {
    if let Ok(result) = InvariantResult::from_vm_value(value) {
        return Ok(result);
    }
    let json = vm_value_to_json(value);
    let Some(object) = json.as_object() else {
        return Err(format!(
            "predicate returned {}, expected dict",
            value.type_name()
        ));
    };
    let verdict = object
        .get("verdict")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "predicate result missing string `verdict`".to_string())?;
    let rule = object
        .get("rule")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("invariant");
    let remediation = object
        .get("remediation")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let finding_count = object
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    match verdict {
        "Allow" | "allow" => Ok(InvariantResult::allow()),
        "Warn" | "warn" => Ok(InvariantResult::warn(non_empty_or_rule(remediation, rule))),
        "Block" | "block" => Ok(InvariantResult::block(InvariantBlockError::new(
            rule,
            if remediation.is_empty() {
                format!("{rule} found {finding_count} finding(s)")
            } else {
                remediation.to_string()
            },
        ))),
        other => Err(format!(
            "predicate result has unsupported verdict `{other}`; expected Allow, Warn, or Block"
        )),
    }
}

fn non_empty_or_rule(value: &str, rule: &str) -> String {
    if value.is_empty() {
        rule.to_string()
    } else {
        value.to_string()
    }
}

fn predicate_error_record(
    name: &str,
    hash: &str,
    kind: PredicateKind,
    code: &str,
    message: impl Into<String>,
) -> serde_json::Value {
    let result = InvariantResult::block(InvariantBlockError::new(code, message));
    serde_json::json!({
        "name": name,
        "hash": hash,
        "kind": predicate_kind_label(kind),
        "result": serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        "raw_result": nil_json(),
        "attempts": 0,
        "replayable": kind == PredicateKind::Deterministic,
    })
}

fn predicate_kind_label(kind: PredicateKind) -> &'static str {
    match kind {
        PredicateKind::Deterministic => "deterministic",
        PredicateKind::Semantic => "semantic",
    }
}

fn nil_json() -> serde_json::Value {
    serde_json::Value::Null
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &FLOW_INVARIANT_ALLOW_IMPL_DEF,
    &FLOW_INVARIANT_WARN_IMPL_DEF,
    &FLOW_INVARIANT_BLOCK_IMPL_DEF,
    &FLOW_INVARIANT_REQUIRE_APPROVAL_IMPL_DEF,
    &FLOW_EVIDENCE_ATOM_IMPL_DEF,
    &FLOW_EVIDENCE_METADATA_IMPL_DEF,
    &FLOW_EVIDENCE_TRANSCRIPT_IMPL_DEF,
    &FLOW_EVIDENCE_CITATION_IMPL_DEF,
    &FLOW_REMEDIATION_IMPL_DEF,
    &FLOW_WITH_EVIDENCE_IMPL_DEF,
    &FLOW_WITH_REMEDIATION_IMPL_DEF,
    &FLOW_WITH_CONFIDENCE_IMPL_DEF,
    &FLOW_INVARIANT_KIND_IMPL_DEF,
    &FLOW_INVARIANT_IS_BLOCKING_IMPL_DEF,
    &FLOW_INVARIANT_CONFIDENCE_IMPL_DEF,
    &FLOW_INVARIANT_FEEDBACK_IMPL_DEF,
];

fn serde_to_vm<T: serde::Serialize>(value: &T) -> VmValue {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    crate::stdlib::json_to_vm_value(&json)
}

fn required_string(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required string argument `{name}`"
        ))),
    }
}

fn required_u64(args: &[VmValue], index: usize, builtin: &str, name: &str) -> Result<u64, VmError> {
    match args.get(index) {
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as u64),
        Some(VmValue::Int(n)) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be non-negative, got {n}"
        ))),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be an int, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required int argument `{name}`"
        ))),
    }
}

fn required_f64(args: &[VmValue], index: usize, builtin: &str, name: &str) -> Result<f64, VmError> {
    match args.get(index) {
        Some(VmValue::Float(n)) => Ok(*n),
        Some(VmValue::Int(n)) => Ok(*n as f64),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a number, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required number argument `{name}`"
        ))),
    }
}

fn require_list_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<&'a [VmValue], VmError> {
    match args.get(index) {
        Some(VmValue::List(items)) => Ok(items.as_slice()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: argument `{name}` must be a list, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: missing required list argument `{name}`"
        ))),
    }
}

fn require_invariant(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<InvariantResult, VmError> {
    let value = args.get(index).ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: missing required invariant result argument"
        ))
    })?;
    InvariantResult::from_vm_value(value)
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn parse_atom_id(hex_str: &str, builtin: &str) -> Result<crate::flow::AtomId, VmError> {
    crate::flow::AtomId::from_hex(hex_str)
        .map_err(|error| VmError::Runtime(format!("{builtin}: invalid atom id: {error}")))
}

fn validate_span(start: u64, end: u64, builtin: &str) -> Result<(), VmError> {
    if end < start {
        return Err(VmError::Runtime(format!(
            "{builtin}: span end ({end}) must be >= start ({start})"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct FlowFeedbackOptions {
    include_findings: bool,
    include_allow: bool,
    include_skipped: bool,
    empty_if_clear: bool,
    max_findings_per_item: usize,
    max_items: usize,
    max_message_chars: usize,
}

impl Default for FlowFeedbackOptions {
    fn default() -> Self {
        Self {
            include_findings: true,
            include_allow: false,
            include_skipped: false,
            empty_if_clear: true,
            max_findings_per_item: DEFAULT_FEEDBACK_MAX_FINDINGS_PER_ITEM,
            max_items: DEFAULT_FEEDBACK_MAX_ITEMS,
            max_message_chars: DEFAULT_FEEDBACK_MAX_MESSAGE_CHARS,
        }
    }
}

impl FlowFeedbackOptions {
    fn parse(value: Option<&VmValue>) -> Result<Self, VmError> {
        let opts = match value {
            None | Some(VmValue::Nil) => return Ok(Self::default()),
            Some(VmValue::Dict(map)) => map.as_ref(),
            Some(other) => {
                return Err(VmError::Runtime(format!(
                    "flow_invariant_feedback: options must be a dict or nil, got {}",
                    other.type_name()
                )));
            }
        };
        const KEYS: &[&str] = &[
            "empty_if_clear",
            "include_allow",
            "include_findings",
            "include_skipped",
            "max_findings_per_item",
            "max_items",
            "max_message_chars",
        ];
        for key in opts.keys() {
            if !KEYS.contains(&key.as_str()) {
                return Err(VmError::Runtime(format!(
                    "flow_invariant_feedback: unknown option key '{key}'"
                )));
            }
        }

        let mut parsed = Self::default();
        parsed.include_allow =
            feedback_bool_option(opts, "include_allow")?.unwrap_or(parsed.include_allow);
        parsed.include_findings =
            feedback_bool_option(opts, "include_findings")?.unwrap_or(parsed.include_findings);
        parsed.include_skipped =
            feedback_bool_option(opts, "include_skipped")?.unwrap_or(parsed.include_skipped);
        parsed.empty_if_clear =
            feedback_bool_option(opts, "empty_if_clear")?.unwrap_or(parsed.empty_if_clear);
        parsed.max_findings_per_item = feedback_usize_option(
            opts,
            "max_findings_per_item",
            DEFAULT_FEEDBACK_MAX_FINDINGS_PER_ITEM,
            HARD_FEEDBACK_MAX_FINDINGS_PER_ITEM,
        )?;
        parsed.max_items = feedback_usize_option(
            opts,
            "max_items",
            DEFAULT_FEEDBACK_MAX_ITEMS,
            HARD_FEEDBACK_MAX_ITEMS,
        )?;
        parsed.max_message_chars = feedback_usize_option(
            opts,
            "max_message_chars",
            DEFAULT_FEEDBACK_MAX_MESSAGE_CHARS,
            HARD_FEEDBACK_MAX_MESSAGE_CHARS,
        )?;
        Ok(parsed)
    }
}

fn feedback_bool_option(opts: &crate::value::DictMap, key: &str) -> Result<Option<bool>, VmError> {
    match opts.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Runtime(format!(
            "flow_invariant_feedback: option `{key}` must be bool, got {}",
            other.type_name()
        ))),
    }
}

fn feedback_usize_option(
    opts: &crate::value::DictMap,
    key: &str,
    default: usize,
    hard_max: usize,
) -> Result<usize, VmError> {
    match opts.get(key) {
        None | Some(VmValue::Nil) => Ok(default),
        Some(VmValue::Int(value)) if *value > 0 => Ok((*value as usize).min(hard_max)),
        Some(VmValue::Int(value)) => Err(VmError::Runtime(format!(
            "flow_invariant_feedback: option `{key}` must be > 0, got {value}"
        ))),
        Some(other) => Err(VmError::Runtime(format!(
            "flow_invariant_feedback: option `{key}` must be an int, got {}",
            other.type_name()
        ))),
    }
}

fn build_flow_feedback(
    report: &serde_json::Map<String, serde_json::Value>,
    options: &FlowFeedbackOptions,
) -> String {
    let mut items = Vec::new();
    let discovery_error = report.get("status").and_then(json_str) == Some("discovery_error");
    if let Some(diagnostics) = report
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
    {
        for diagnostic in diagnostics {
            if let Some(item) = diagnostic_feedback_item(diagnostic, discovery_error, options) {
                items.push(item);
            }
        }
    }
    if let Some(records) = report.get("records").and_then(serde_json::Value::as_array) {
        for record in records {
            if let Some(item) = record_feedback_item(record, options) {
                items.push(item);
            }
        }
    }
    if options.include_skipped {
        if let Some(skipped) = report.get("skipped").and_then(serde_json::Value::as_array) {
            for record in skipped {
                if let Some(item) = skipped_feedback_item(record, options) {
                    items.push(item);
                }
            }
        }
    }

    if items.is_empty() {
        if options.empty_if_clear {
            return String::new();
        }
        return "Flow invariants passed.".to_string();
    }

    let mut lines = vec!["Flow invariants need attention:".to_string()];
    let omitted = items.len().saturating_sub(options.max_items);
    lines.extend(items.into_iter().take(options.max_items));
    if omitted > 0 {
        lines.push(format!("- {omitted} more item(s) omitted."));
    }
    lines.join("\n")
}

fn diagnostic_feedback_item(
    diagnostic: &serde_json::Value,
    discovery_error: bool,
    options: &FlowFeedbackOptions,
) -> Option<String> {
    let severity = diagnostic
        .get("severity")
        .and_then(json_str)
        .unwrap_or("diagnostic");
    if !discovery_error && severity != "error" {
        return None;
    }
    let message = diagnostic
        .get("message")
        .and_then(json_str)
        .unwrap_or("unknown diagnostic");
    Some(format!(
        "- Diagnostic {severity}: {}",
        feedback_compact(message, options.max_message_chars)
    ))
}

fn record_feedback_item(
    record: &serde_json::Value,
    options: &FlowFeedbackOptions,
) -> Option<String> {
    let result = record.get("result")?;
    let verdict = result.get("verdict")?;
    let verdict_kind = verdict.get("kind").and_then(json_str).unwrap_or("unknown");
    if verdict_kind == "allow" && !options.include_allow {
        return None;
    }

    let name = record
        .get("name")
        .and_then(json_str)
        .or_else(|| record.get("hash").and_then(json_str))
        .unwrap_or("unnamed_predicate");
    let message = match verdict_kind {
        "allow" => "passed".to_string(),
        "warn" => verdict
            .get("reason")
            .and_then(json_str)
            .unwrap_or("warning")
            .to_string(),
        "block" => {
            let error = verdict.get("error");
            let code = error
                .and_then(|value| value.get("code"))
                .and_then(json_str)
                .unwrap_or("block");
            let detail = error
                .and_then(|value| value.get("message"))
                .and_then(json_str)
                .unwrap_or("blocked");
            format!("{code}: {detail}")
        }
        "require_approval" => {
            let approver = verdict.get("approver");
            let approver_kind = approver
                .and_then(|value| value.get("kind"))
                .and_then(json_str)
                .unwrap_or("approver");
            let approver_id = approver
                .and_then(|value| value.get("id").or_else(|| value.get("name")))
                .and_then(json_str)
                .unwrap_or("unknown");
            format!("requires {approver_kind} approval from {approver_id}")
        }
        other => format!("unsupported verdict {other}"),
    };
    let mut line = format!(
        "- {} {}: {}",
        feedback_label(verdict_kind),
        name,
        feedback_compact(&message, options.max_message_chars)
    );
    if let Some(remediation) = remediation_text(result, record) {
        let remediation = feedback_compact(remediation, options.max_message_chars);
        if !remediation.is_empty() {
            line.push_str(" Remediation: ");
            line.push_str(&remediation);
        }
    }
    if let Some(findings) = findings_text(record, options) {
        line.push_str(" Findings: ");
        line.push_str(&findings);
    }
    Some(line)
}

fn skipped_feedback_item(
    record: &serde_json::Value,
    options: &FlowFeedbackOptions,
) -> Option<String> {
    let name = record
        .get("name")
        .and_then(json_str)
        .or_else(|| record.get("hash").and_then(json_str))
        .unwrap_or("unnamed_predicate");
    let reason = record.get("reason").and_then(json_str).unwrap_or("skipped");
    Some(format!(
        "- Skipped {}: {}",
        name,
        feedback_compact(reason, options.max_message_chars)
    ))
}

fn feedback_label(verdict_kind: &str) -> &'static str {
    match verdict_kind {
        "allow" => "Allow",
        "warn" => "Warn",
        "block" => "Block",
        "require_approval" => "RequireApproval",
        _ => "Invariant",
    }
}

fn remediation_text<'a>(
    result: &'a serde_json::Value,
    record: &'a serde_json::Value,
) -> Option<&'a str> {
    result
        .get("remediation")
        .and_then(|value| {
            value
                .get("description")
                .and_then(json_str)
                .or_else(|| json_str(value))
        })
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            record
                .get("raw_result")
                .and_then(|value| value.get("remediation"))
                .and_then(json_str)
                .filter(|value| !value.trim().is_empty())
        })
}

fn findings_text(record: &serde_json::Value, options: &FlowFeedbackOptions) -> Option<String> {
    if !options.include_findings {
        return None;
    }
    let findings = record
        .get("raw_result")
        .and_then(|value| value.get("findings"))
        .or_else(|| record.get("findings"))
        .and_then(serde_json::Value::as_array)?;
    let mut labels = Vec::new();
    let mut omitted = 0usize;
    for finding in findings {
        let Some(label) = finding_label(finding, options.max_message_chars) else {
            continue;
        };
        if labels.iter().any(|existing| existing == &label) {
            continue;
        }
        if labels.len() >= options.max_findings_per_item {
            omitted += 1;
            continue;
        }
        labels.push(label);
    }
    if labels.is_empty() {
        return None;
    }
    let mut text = labels.join(", ");
    if omitted > 0 {
        text.push_str(&format!(" (+{omitted} more)"));
    }
    Some(text)
}

fn finding_label(finding: &serde_json::Value, max_chars: usize) -> Option<String> {
    if let Some(text) = json_str(finding) {
        return Some(feedback_compact(text, max_chars));
    }
    let object = finding.as_object()?;
    if let Some(path) = object.get("path").and_then(json_str) {
        let mut label = path.to_string();
        if let Some(line) = object
            .get("line")
            .or_else(|| object.get("start_line"))
            .and_then(serde_json::Value::as_u64)
        {
            label.push(':');
            label.push_str(&line.to_string());
        }
        return Some(feedback_compact(&label, max_chars));
    }
    for key in ["message", "reason", "text"] {
        if let Some(text) = object.get(key).and_then(json_str) {
            return Some(feedback_compact(text, max_chars));
        }
    }
    None
}

fn json_str(value: &serde_json::Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

fn feedback_compact(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let keep = max_chars.saturating_sub(3).max(1);
    let mut out = compact.chars().take(keep).collect::<String>();
    out.push_str("...");
    out
}

fn decode_evidence_item(value: &VmValue) -> Result<EvidenceItem, VmError> {
    let json = value_to_json(value);
    serde_json::from_value(json)
        .map_err(|error| VmError::Runtime(format!("flow_with_evidence: invalid evidence: {error}")))
}

fn decode_remediation(value: &VmValue) -> Result<Remediation, VmError> {
    let json = value_to_json(value);
    serde_json::from_value(json).map_err(|error| {
        VmError::Runtime(format!(
            "flow_with_remediation: invalid remediation: {error}"
        ))
    })
}

fn value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::Value::from(*n),
        VmValue::Float(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        VmValue::Dict(map) => {
            let mut object = BTreeMap::new();
            for (key, item) in map.iter() {
                object.insert(key.to_string(), value_to_json(item));
            }
            serde_json::Value::Object(object.into_iter().collect())
        }
        other => serde_json::Value::String(other.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;

    fn vm_with_flow_builtins() -> Vm {
        let mut vm = Vm::new();
        register_flow_builtins(&mut vm);
        vm
    }

    fn call(vm: &Vm, name: &str, args: &[VmValue]) -> VmValue {
        let mut out = String::new();
        let builtin = vm
            .builtins
            .get(name)
            .unwrap_or_else(|| panic!("builtin {name} not registered"))
            .clone();
        builtin(args, &mut out).expect("builtin call failed")
    }

    #[test]
    fn flow_invariant_allow_returns_dict_round_trippable() {
        let vm = vm_with_flow_builtins();
        let result = call(&vm, "flow_invariant_allow", &[]);
        let decoded = InvariantResult::from_vm_value(&result).unwrap();
        assert_eq!(decoded, InvariantResult::allow());
    }

    #[test]
    fn flow_invariant_warn_carries_reason() {
        let vm = vm_with_flow_builtins();
        let result = call(
            &vm,
            "flow_invariant_warn",
            &[VmValue::String(arcstr::ArcStr::from("untested helper"))],
        );
        let decoded = InvariantResult::from_vm_value(&result).unwrap();
        match decoded.verdict {
            Verdict::Warn { reason } => assert_eq!(reason, "untested helper"),
            other => panic!("expected warn verdict, got {other:?}"),
        }
    }

    #[test]
    fn flow_invariant_block_carries_code_and_message() {
        let vm = vm_with_flow_builtins();
        let result = call(
            &vm,
            "flow_invariant_block",
            &[
                VmValue::String(arcstr::ArcStr::from("missing_test")),
                VmValue::String(arcstr::ArcStr::from("no test covers this atom")),
            ],
        );
        let decoded = InvariantResult::from_vm_value(&result).unwrap();
        assert!(decoded.is_blocking());
        let error = decoded.block_error().unwrap();
        assert_eq!(error.code, "missing_test");
        assert_eq!(error.message, "no test covers this atom");
    }

    #[test]
    fn flow_invariant_require_approval_routes_to_principal_or_role() {
        let vm = vm_with_flow_builtins();
        let principal_value = call(
            &vm,
            "flow_invariant_require_approval",
            &[
                VmValue::String(arcstr::ArcStr::from("principal")),
                VmValue::String(arcstr::ArcStr::from("user:alice")),
            ],
        );
        let role_value = call(
            &vm,
            "flow_invariant_require_approval",
            &[
                VmValue::String(arcstr::ArcStr::from("role")),
                VmValue::String(arcstr::ArcStr::from("security-reviewer")),
            ],
        );
        let principal = InvariantResult::from_vm_value(&principal_value).unwrap();
        let role = InvariantResult::from_vm_value(&role_value).unwrap();
        assert_eq!(
            principal.verdict,
            Verdict::RequireApproval {
                approver: Approver::principal("user:alice"),
            }
        );
        assert_eq!(
            role.verdict,
            Verdict::RequireApproval {
                approver: Approver::role("security-reviewer"),
            }
        );
    }

    #[test]
    fn flow_invariant_require_approval_rejects_unknown_kind() {
        let vm = vm_with_flow_builtins();
        let mut out = String::new();
        let builtin = vm
            .builtins
            .get("flow_invariant_require_approval")
            .unwrap()
            .clone();
        let error = builtin(
            &[
                VmValue::String(arcstr::ArcStr::from("squad")),
                VmValue::String(arcstr::ArcStr::from("ship-captains")),
            ],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("kind must be"));
    }

    #[test]
    fn flow_with_evidence_attaches_all_four_evidence_kinds() {
        let vm = vm_with_flow_builtins();
        let allow = call(&vm, "flow_invariant_allow", &[]);
        let atom_hex = "01".repeat(32);
        let atom_evidence = call(
            &vm,
            "flow_evidence_atom",
            &[
                VmValue::String(arcstr::ArcStr::from(atom_hex.as_str())),
                VmValue::Int(0),
                VmValue::Int(64),
            ],
        );
        let metadata_evidence = call(
            &vm,
            "flow_evidence_metadata",
            &[
                VmValue::String(arcstr::ArcStr::from("src/auth")),
                VmValue::String(arcstr::ArcStr::from("policy")),
                VmValue::String(arcstr::ArcStr::from("min_review_count")),
            ],
        );
        let transcript_evidence = call(
            &vm,
            "flow_evidence_transcript",
            &[
                VmValue::String(arcstr::ArcStr::from("transcript-0001")),
                VmValue::Int(128),
                VmValue::Int(256),
            ],
        );
        let citation_evidence = call(
            &vm,
            "flow_evidence_citation",
            &[
                VmValue::String(arcstr::ArcStr::from("https://harnlang.com/spec")),
                VmValue::String(arcstr::ArcStr::from("verdicts may grade")),
                VmValue::String(arcstr::ArcStr::from("2026-04-26T00:00:00Z")),
            ],
        );
        let evidence_list = VmValue::List(std::sync::Arc::new(vec![
            atom_evidence,
            metadata_evidence,
            transcript_evidence,
            citation_evidence,
        ]));
        let attached = call(&vm, "flow_with_evidence", &[allow, evidence_list]);
        let decoded = InvariantResult::from_vm_value(&attached).unwrap();
        assert_eq!(decoded.evidence.len(), 4);
        assert!(matches!(
            decoded.evidence[0],
            EvidenceItem::AtomPointer { .. }
        ));
        assert!(matches!(
            decoded.evidence[1],
            EvidenceItem::MetadataPath { .. }
        ));
        assert!(matches!(
            decoded.evidence[2],
            EvidenceItem::TranscriptExcerpt { .. }
        ));
        assert!(matches!(
            decoded.evidence[3],
            EvidenceItem::ExternalCitation { .. }
        ));
    }

    #[test]
    fn flow_with_remediation_attaches_description() {
        let vm = vm_with_flow_builtins();
        let block = call(
            &vm,
            "flow_invariant_block",
            &[
                VmValue::String(arcstr::ArcStr::from("style")),
                VmValue::String(arcstr::ArcStr::from("trailing whitespace")),
            ],
        );
        let remediation = call(
            &vm,
            "flow_remediation",
            &[VmValue::String(arcstr::ArcStr::from(
                "strip trailing whitespace",
            ))],
        );
        let attached = call(&vm, "flow_with_remediation", &[block, remediation]);
        let decoded = InvariantResult::from_vm_value(&attached).unwrap();
        assert_eq!(
            decoded.remediation.unwrap().description,
            "strip trailing whitespace"
        );
    }

    #[test]
    fn flow_with_confidence_clamps_to_unit_interval() {
        let vm = vm_with_flow_builtins();
        let warn = call(
            &vm,
            "flow_invariant_warn",
            &[VmValue::String(arcstr::ArcStr::from("low signal"))],
        );
        let attached = call(&vm, "flow_with_confidence", &[warn, VmValue::Float(1.5)]);
        let decoded = InvariantResult::from_vm_value(&attached).unwrap();
        assert_eq!(decoded.confidence, 1.0);
    }

    #[test]
    fn flow_invariant_kind_returns_string_label() {
        let vm = vm_with_flow_builtins();
        let allow = call(&vm, "flow_invariant_allow", &[]);
        let kind = call(&vm, "flow_invariant_kind", &[allow]);
        match kind {
            VmValue::String(s) => assert_eq!(s.as_str(), "allow"),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn flow_invariant_feedback_summarizes_non_allow_records() {
        let vm = vm_with_flow_builtins();
        let report = json_to_vm_value(&serde_json::json!({
            "ok": false,
            "status": "fail",
            "records": [
                {
                    "name": "no_bad",
                    "kind": "deterministic",
                    "result": {
                        "verdict": {
                            "kind": "block",
                            "error": {
                                "code": "no_bad",
                                "message": "bad sentinel text remains"
                            }
                        },
                        "confidence": 1.0
                    },
                    "raw_result": {
                        "remediation": "Remove bad sentinel text.",
                        "findings": [
                            {"path": "src/main.zig", "pattern": "bad"},
                            {"path": "src/lib.zig", "line": 42},
                            {"path": "src/extra.zig"},
                            {"path": "src/omitted.zig"}
                        ]
                    }
                },
                {
                    "name": "style_nit",
                    "kind": "deterministic",
                    "result": {
                        "verdict": {
                            "kind": "warn",
                            "reason": "comment is stale"
                        },
                        "remediation": {
                            "description": "Update the comment."
                        },
                        "confidence": 1.0
                    }
                },
                {
                    "name": "passed",
                    "kind": "deterministic",
                    "result": {
                        "verdict": {"kind": "allow"},
                        "confidence": 1.0
                    }
                }
            ],
            "skipped": []
        }));
        let feedback = call(&vm, "flow_invariant_feedback", &[report]);
        let VmValue::String(text) = feedback else {
            panic!("expected feedback string");
        };

        assert!(text.contains("Flow invariants need attention:"));
        assert!(text.contains("- Block no_bad: no_bad: bad sentinel text remains"));
        assert!(text.contains("Remediation: Remove bad sentinel text."));
        assert!(text.contains("Findings: src/main.zig, src/lib.zig:42, src/extra.zig (+1 more)"));
        assert!(text.contains("- Warn style_nit: comment is stale"));
        assert!(text.contains("Remediation: Update the comment."));
        assert!(!text.contains("passed"));
    }

    #[test]
    fn flow_invariant_feedback_can_omit_findings() {
        let vm = vm_with_flow_builtins();
        let report = json_to_vm_value(&serde_json::json!({
            "ok": false,
            "status": "fail",
            "records": [{
                "name": "no_bad",
                "kind": "deterministic",
                "result": {
                    "verdict": {
                        "kind": "block",
                        "error": {
                            "code": "no_bad",
                            "message": "bad sentinel text remains"
                        }
                    },
                    "confidence": 1.0
                },
                "raw_result": {
                    "findings": [{"path": "src/main.zig"}]
                }
            }],
            "skipped": []
        }));
        let options = json_to_vm_value(&serde_json::json!({"include_findings": false}));
        let feedback = call(&vm, "flow_invariant_feedback", &[report, options]);
        let VmValue::String(text) = feedback else {
            panic!("expected feedback string");
        };

        assert!(text.contains("- Block no_bad: no_bad: bad sentinel text remains"));
        assert!(!text.contains("Findings:"));
        assert!(!text.contains("src/main.zig"));
    }

    #[test]
    fn flow_invariant_feedback_caps_findings_per_item() {
        let vm = vm_with_flow_builtins();
        let report = json_to_vm_value(&serde_json::json!({
            "ok": false,
            "status": "fail",
            "records": [{
                "name": "no_bad",
                "kind": "deterministic",
                "result": {
                    "verdict": {
                        "kind": "block",
                        "error": {
                            "code": "no_bad",
                            "message": "bad sentinel text remains"
                        }
                    },
                    "confidence": 1.0
                },
                "raw_result": {
                    "findings": [
                        {"path": "src/main.zig"},
                        {"path": "src/main.zig"},
                        {"pattern": "not enough to label"},
                        "src/lib.zig",
                        {"message": "src/extra.zig"}
                    ]
                }
            }],
            "skipped": []
        }));
        let options = json_to_vm_value(&serde_json::json!({"max_findings_per_item": 1}));
        let feedback = call(&vm, "flow_invariant_feedback", &[report, options]);
        let VmValue::String(text) = feedback else {
            panic!("expected feedback string");
        };

        assert!(text.contains("Findings: src/main.zig (+2 more)"));
        assert!(!text.contains("src/lib.zig"));
        assert!(!text.contains("src/extra.zig"));
        assert!(!text.contains("not enough to label"));
    }

    #[test]
    fn flow_invariant_feedback_is_empty_when_clear_by_default() {
        let vm = vm_with_flow_builtins();
        let report = json_to_vm_value(&serde_json::json!({
            "ok": true,
            "status": "pass",
            "records": [{
                "name": "passed",
                "result": {
                    "verdict": {"kind": "allow"},
                    "confidence": 1.0
                }
            }],
            "skipped": []
        }));
        let feedback = call(&vm, "flow_invariant_feedback", &[report]);

        match feedback {
            VmValue::String(text) => assert_eq!(text.as_str(), ""),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn flow_invariant_feedback_can_include_skipped_records() {
        let vm = vm_with_flow_builtins();
        let report = json_to_vm_value(&serde_json::json!({
            "ok": true,
            "status": "pass",
            "records": [],
            "skipped": [{
                "name": "semantic_review",
                "kind": "semantic",
                "reason": "semantic predicates require an explicit include_semantic option"
            }]
        }));
        let options = json_to_vm_value(&serde_json::json!({
            "include_skipped": true,
            "empty_if_clear": false
        }));
        let feedback = call(&vm, "flow_invariant_feedback", &[report, options]);
        let VmValue::String(text) = feedback else {
            panic!("expected feedback string");
        };

        assert!(text.contains("- Skipped semantic_review: semantic predicates require"));
    }

    #[test]
    fn flow_invariant_feedback_summarizes_discovery_errors() {
        let vm = vm_with_flow_builtins();
        let report = json_to_vm_value(&serde_json::json!({
            "ok": false,
            "status": "discovery_error",
            "diagnostics": [
                {"severity": "warning", "message": "missing archivist metadata"},
                {"severity": "error", "message": "semantic fallback is unresolved"}
            ],
            "records": [],
            "skipped": []
        }));
        let feedback = call(&vm, "flow_invariant_feedback", &[report]);
        let VmValue::String(text) = feedback else {
            panic!("expected feedback string");
        };

        assert!(text.contains("- Diagnostic warning: missing archivist metadata"));
        assert!(text.contains("- Diagnostic error: semantic fallback is unresolved"));
    }

    #[test]
    fn flow_evidence_atom_rejects_invalid_span() {
        let vm = vm_with_flow_builtins();
        let mut out = String::new();
        let builtin = vm.builtins.get("flow_evidence_atom").unwrap().clone();
        let atom_hex = "ab".repeat(32);
        let error = builtin(
            &[
                VmValue::String(arcstr::ArcStr::from(atom_hex.as_str())),
                VmValue::Int(64),
                VmValue::Int(32),
            ],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("must be >="));
    }

    #[test]
    fn flow_evidence_atom_rejects_bad_hex() {
        let vm = vm_with_flow_builtins();
        let mut out = String::new();
        let builtin = vm.builtins.get("flow_evidence_atom").unwrap().clone();
        let error = builtin(
            &[
                VmValue::String(arcstr::ArcStr::from("not-hex")),
                VmValue::Int(0),
                VmValue::Int(8),
            ],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("invalid atom id"));
    }

    async fn eval_source(source: &str, slice: serde_json::Value) -> serde_json::Value {
        let mut vm = vm_with_flow_builtins();
        let value = vm
            .call_named_builtin(
                "flow_evaluate_invariants",
                vec![
                    VmValue::String(arcstr::ArcStr::from(source)),
                    json_to_vm_value(&slice),
                ],
            )
            .await
            .unwrap();
        vm_value_to_json(&value)
    }

    #[tokio::test]
    async fn flow_evaluate_invariants_adapts_harn_canon_result_shape() {
        let report = eval_source(
            r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn no_bad(slice, _ctx, _repo_at_base) {
  return {
    verdict: "Block",
    rule: "no_bad",
    findings: [{path: slice.files[0].path}],
    remediation: "Remove bad sentinel text.",
  }
}
"#,
            serde_json::json!({
                "files": [{"path": "src/lib.rs", "text": "bad"}],
            }),
        )
        .await;

        assert_eq!(report["ok"], false);
        assert_eq!(report["status"], "fail");
        assert_eq!(report["records"][0]["name"], "no_bad");
        assert_eq!(
            report["records"][0]["result"]["verdict"]["error"]["code"],
            "no_bad"
        );
        assert_eq!(
            report["records"][0]["raw_result"]["findings"][0]["path"],
            "src/lib.rs"
        );
    }

    #[tokio::test]
    async fn flow_evaluate_invariants_accepts_typed_flow_results() {
        let report = eval_source(
            r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn typed_block(_slice, _ctx, _repo_at_base) {
  return flow_invariant_block("typed_rule", "Typed Flow result blocked.")
}
"#,
            serde_json::json!({"files": []}),
        )
        .await;

        assert_eq!(report["ok"], false);
        assert_eq!(
            report["records"][0]["result"]["verdict"]["error"]["code"],
            "typed_rule"
        );
        assert_eq!(
            report["records"][0]["raw_result"]["verdict"]["kind"],
            "block"
        );
    }

    #[tokio::test]
    async fn flow_evaluate_invariants_skips_semantic_by_default() {
        let report = eval_source(
            r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn fallback(_slice, _ctx, _repo_at_base) {
  return {verdict: "Allow", rule: "fallback", findings: [], remediation: ""}
}

@invariant
@semantic(fallback: "fallback")
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-06-28")
pub fn semantic_check(_slice, _ctx, _repo_at_base) {
  return {verdict: "Block", rule: "semantic_check", findings: [], remediation: "should not run"}
}
"#,
            serde_json::json!({"files": []}),
        )
        .await;

        assert_eq!(report["ok"], true);
        assert_eq!(report["records"][0]["name"], "fallback");
        assert_eq!(report["skipped"][0]["name"], "semantic_check");
        assert_eq!(
            report["skipped"][0]["reason"],
            "semantic predicates require an explicit include_semantic option"
        );
    }
}
