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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::flow::{
    parse_invariants_source, Approver, AtomId, ByteSpan, EvidenceItem, InvariantBlockError,
    InvariantResult, PredicateExecutor, PredicateExecutorConfig, PredicateHash, PredicateKind,
    PredicateRunner, Remediation, SemanticFallbackPolicy, Slice, SliceId, SliceStatus, Verdict,
};
use crate::llm::helpers::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm, VmBuiltinArity, VmBuiltinMetadata};
use async_trait::async_trait;

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
            "code": discovery_diagnostic_code(&diagnostic.message),
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

    let predicates = parsed.predicates;
    let by_name = predicates
        .iter()
        .cloned()
        .map(|predicate| (predicate.name.clone(), predicate))
        .collect::<BTreeMap<_, _>>();
    let requested_names = request
        .predicate_names
        .as_ref()
        .map(|names| names.iter().cloned().collect::<BTreeSet<_>>());
    let mut vm = ctx.child_vm();
    let exports = match request.module_path.clone() {
        Some(path) => vm.load_module_exports(&path).await?,
        None => {
            let source = request.source.clone();
            vm.load_module_exports_from_source(
                request
                    .source_key
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("<flow-evaluate-invariants>.harn")),
                &source,
            )
            .await?
        }
    };
    let vm = Arc::new(tokio::sync::Mutex::new(vm));
    let mut runners: Vec<Arc<dyn PredicateRunner>> = Vec::new();
    for predicate in predicates {
        let closure = exports.get(&predicate.name).cloned();
        let fallback_hash = predicate
            .fallback
            .as_ref()
            .and_then(|name| by_name.get(name))
            .map(|fallback| fallback.source_hash.clone());
        let fallback_diagnostic = if predicate.kind != PredicateKind::Semantic {
            None
        } else {
            match predicate.fallback.as_ref() {
                None => Some(InvariantBlockError::new(
                    "fallback_missing",
                    "semantic predicate did not declare a deterministic fallback",
                )),
                Some(name) if !by_name.contains_key(name) => Some(InvariantBlockError::new(
                    "fallback_unresolved",
                    format!("semantic predicate fallback `{name}` could not be resolved"),
                )),
                Some(_) => None,
            }
        };
        runners.push(Arc::new(HarnPredicateRunner {
            name: predicate.name.clone(),
            hash: predicate.source_hash.clone(),
            kind: predicate.kind,
            fallback_hash,
            fallback_policy: predicate.fallback_policy,
            fallback_diagnostic,
            vm: vm.clone(),
            closure,
            args: [
                request.slice.clone(),
                request.predicate_ctx.clone(),
                request.repo_at_base.clone(),
            ],
            raw_result: Mutex::new(None),
        }));
    }

    let executor = PredicateExecutor::new(PredicateExecutorConfig {
        deterministic_budget: request.budget,
        semantic_budget: request.budget,
        replay_deterministic: false,
        ..PredicateExecutorConfig::default()
    });
    let report = executor
        .execute_named_slice_serial(
            &opaque_flow_slice(),
            &runners,
            requested_names.as_ref(),
            request.include_semantic,
        )
        .await;
    let ok = report.is_allowed();
    let skipped = serde_json::to_value(&report.skipped)
        .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
    let records = serde_json::to_value(&report.records)
        .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
    let records = records.as_array().cloned().unwrap_or_default();
    for record in &records {
        let Some(code) = record
            .get("result")
            .and_then(|result| result.get("verdict"))
            .and_then(|verdict| verdict.get("error"))
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if code.starts_with("fallback_") {
            diagnostics.push(serde_json::json!({
                "severity": "error",
                "code": code,
                "predicate": record.get("name"),
                "message": record
                    .get("result")
                    .and_then(|result| result.get("verdict"))
                    .and_then(|verdict| verdict.get("error"))
                    .and_then(|error| error.get("message")),
            }));
        }
    }
    Ok(json_to_vm_value(&serde_json::json!({
        "ok": ok,
        "status": if ok { "pass" } else { "fail" },
        "diagnostics": diagnostics,
        "records": records,
        "skipped": skipped,
    })))
}

struct HarnPredicateRunner {
    name: String,
    hash: PredicateHash,
    kind: PredicateKind,
    fallback_hash: Option<PredicateHash>,
    fallback_policy: SemanticFallbackPolicy,
    fallback_diagnostic: Option<InvariantBlockError>,
    vm: Arc<tokio::sync::Mutex<Vm>>,
    closure: Option<Arc<VmClosure>>,
    args: [VmValue; 3],
    raw_result: Mutex<Option<serde_json::Value>>,
}

#[async_trait]
impl PredicateRunner for HarnPredicateRunner {
    fn hash(&self) -> PredicateHash {
        self.hash.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> PredicateKind {
        self.kind
    }

    fn fallback_hash(&self) -> Option<PredicateHash> {
        self.fallback_hash.clone()
    }

    fn fallback_policy(&self) -> SemanticFallbackPolicy {
        self.fallback_policy
    }

    fn fallback_diagnostic(&self) -> Option<InvariantBlockError> {
        self.fallback_diagnostic.clone()
    }

    fn raw_result(&self) -> Option<serde_json::Value> {
        self.raw_result
            .lock()
            .expect("Harn predicate raw result lock")
            .clone()
    }

    async fn evaluate(&self, _context: crate::flow::PredicateContext) -> InvariantResult {
        let Some(closure) = self.closure.as_ref() else {
            return InvariantResult::block(InvariantBlockError::new(
                "predicate_missing_export",
                format!("invariant `{}` was parsed but not exported", self.name),
            ));
        };
        match self
            .vm
            .lock()
            .await
            .call_closure_pub(closure, &self.args)
            .await
        {
            Ok(value) => {
                *self
                    .raw_result
                    .lock()
                    .expect("Harn predicate raw result lock") = Some(vm_value_to_json(&value));
                adapt_invariant_result(&value).unwrap_or_else(|message| {
                    InvariantResult::block(InvariantBlockError::new(
                        "invalid_predicate_result",
                        message,
                    ))
                })
            }
            Err(error) => InvariantResult::block(InvariantBlockError::new(
                "predicate_runtime_error",
                error.to_string(),
            )),
        }
    }
}

fn opaque_flow_slice() -> Slice {
    Slice {
        id: SliceId([0; 32]),
        atoms: Vec::new(),
        intents: Vec::new(),
        invariants_applied: Vec::new(),
        required_tests: Vec::new(),
        approval_chain: Vec::new(),
        base_ref: AtomId([0; 32]),
        status: SliceStatus::Empty,
    }
}

struct InvariantEvalRequest {
    source: String,
    source_key: Option<PathBuf>,
    module_path: Option<PathBuf>,
    slice: VmValue,
    predicate_ctx: VmValue,
    repo_at_base: VmValue,
    predicate_names: Option<Vec<String>>,
    include_semantic: bool,
    budget: Duration,
}

impl InvariantEvalRequest {
    fn parse(args: &[VmValue]) -> Result<Self, VmError> {
        let option_values = args.get(2).and_then(VmValue::as_dict);
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
        let slice = args.get(1).cloned().ok_or_else(|| {
            VmError::Runtime(
                "flow_evaluate_invariants: missing required slice argument".to_string(),
            )
        })?;
        let predicate_ctx = option_values
            .and_then(|map| map.get("ctx").or_else(|| map.get("predicate_ctx")))
            .cloned()
            .unwrap_or_else(|| json_to_vm_value(&serde_json::json!({})));
        let repo_at_base = option_values
            .and_then(|map| map.get("repo_at_base"))
            .cloned()
            .unwrap_or(VmValue::Nil);
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

fn discovery_diagnostic_code(message: &str) -> &'static str {
    if message.contains("must declare a deterministic fallback") {
        "fallback_missing"
    } else if message.contains("unsupported fallback policy") {
        "fallback_policy_invalid"
    } else {
        "predicate_discovery_error"
    }
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
#[path = "flow_tests.rs"]
mod tests;
