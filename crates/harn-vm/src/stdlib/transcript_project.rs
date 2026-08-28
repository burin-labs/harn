//! Transcript projection policies.
//!
//! Projection is the read-side dual of compaction: it derives a clean
//! model-visible message prefix from an immutable raw transcript without
//! destroying audit lineage. The raw event list and the persisted
//! `messages` array stay untouched; projection just chooses which slice
//! of them the next provider request will see and emits a
//! `transcript.projection` event recording the decision so replay and
//! observability can reconstruct both views.
//!
//! Provider safety: messages carrying signed/opaque reasoning blocks
//! (Anthropic `thinking` with a `signature`) are never rewritten —
//! dropping them would invalidate the provider contract. We surface the
//! conflict through the projection event's `provider_safety_blocked`
//! flag and pass the original message through.

use crate::canonical_json;
use crate::value::VmDictExt;
use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::llm::helpers::{
    is_transcript_value, transcript_event, transcript_message_list, vm_value_to_json,
};
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

#[path = "transcript_project_reachability_gc.rs"]
mod reachability_gc;

/// Canonical `kind` for the transcript event emitted on each projection.
pub(crate) const TRANSCRIPT_PROJECTION_EVENT_KIND: &str = "transcript.projection";
const DEFAULT_REACHABILITY_GC_ROOT_WINDOW: usize = 8;
const DEFAULT_REACHABILITY_GC_MIN_CHARS: usize = 500;

pub(crate) fn register_transcript_projection_builtins(vm: &mut Vm) {
    vm.register_builtin_def(&TRANSCRIPT_PROJECT_IMPL_DEF);
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "transcript_project(transcript: dict | list, options?: any) -> dict",
    kind = "async",
    category = "transcript"
)]
async fn transcript_project_impl(
    ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let transcript_value = args.first().cloned().unwrap_or(VmValue::Nil);
    let transcript = transcript_value
        .as_dict()
        .filter(|_| is_transcript_value(&transcript_value))
        .ok_or_else(|| {
            VmError::Runtime("transcript_project: first argument must be a transcript".into())
        })?;
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let policy = parse_projection_options(&options)?;
    let result = project_transcript(Some(&ctx), transcript, &policy).await?;
    Ok(result_to_vm(&result, &policy))
}

/// Resolved projection policy.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionPolicy {
    pub kind: PolicyKind,
    /// Skip projection if a signed thinking block would be rewritten. The
    /// default is `true` — provider contracts win. Hosts can disable this
    /// when they know the projection is purely additive (e.g. for
    /// previewing a candidate clean view in their own UI without
    /// re-sending to the provider).
    pub respect_provider_signatures: bool,
    /// Reason override; defaults to a policy-specific string.
    pub reason: Option<String>,
    /// For `SummaryPrefix`: how many trailing messages to keep verbatim.
    pub summary_keep_last: usize,
    /// For `SummaryPrefix`: explicit summary text. When `None` we use
    /// `transcript.summary` or, failing that, a deterministic fallback.
    pub summary_text: Option<String>,
    /// For `Custom`: the user closure.
    pub custom: Option<VmValue>,
    /// For `ReachabilityGc`: number of recent messages treated as roots.
    pub gc_root_window: usize,
    /// For `ReachabilityGc`: minimum tool-result body size eligible for reclamation.
    pub gc_min_chars: usize,
    /// For `ReachabilityGc`: labels of root sources consulted for audit metadata.
    pub gc_root_labels: Vec<String>,
    /// For `ReachabilityGc`: additional caller-supplied root text.
    pub gc_root_texts: Vec<String>,
    /// For `ReachabilityGc`: require explicit write-barrier refs before reclaiming.
    pub gc_require_write_barrier: bool,
    /// For `ReachabilityGc`: whether explicit write-barrier refs were supplied.
    pub gc_has_write_barrier: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyKind {
    Raw,
    CleanToolRepair,
    SquashFailedCalls,
    SummaryPrefix,
    ReachabilityGc,
    Custom,
}

impl PolicyKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            PolicyKind::Raw => "raw",
            PolicyKind::CleanToolRepair => "clean_tool_repair",
            PolicyKind::SquashFailedCalls => "squash_failed_calls",
            PolicyKind::SummaryPrefix => "summary_prefix",
            PolicyKind::ReachabilityGc => "reachability_gc",
            PolicyKind::Custom => "custom",
        }
    }
}

impl ProjectionPolicy {
    #[cfg(test)]
    fn default_for(kind: PolicyKind) -> Self {
        Self {
            kind,
            respect_provider_signatures: true,
            reason: None,
            summary_keep_last: 0,
            summary_text: None,
            custom: None,
            gc_root_window: DEFAULT_REACHABILITY_GC_ROOT_WINDOW,
            gc_min_chars: DEFAULT_REACHABILITY_GC_MIN_CHARS,
            gc_root_labels: Vec::new(),
            gc_root_texts: Vec::new(),
            gc_require_write_barrier: false,
            gc_has_write_barrier: false,
        }
    }
}

pub(crate) fn parse_projection_options(options: &VmValue) -> Result<ProjectionPolicy, VmError> {
    let dict = match options {
        VmValue::Nil => None,
        VmValue::Dict(d) => Some((**d).clone()),
        VmValue::String(_) => None,
        _ => {
            return Err(VmError::Runtime(
                "transcript_project: options must be a dict, string, or nil".into(),
            ))
        }
    };
    let kind_str = match options {
        VmValue::String(s) => s.to_string(),
        _ => dict
            .as_ref()
            .and_then(|d| {
                d.get("policy")
                    .or_else(|| d.get("kind"))
                    .or_else(|| d.get("strategy"))
            })
            .and_then(|value| match value {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "raw".to_string()),
    };
    let kind = match kind_str.as_str() {
        "raw" | "identity" | "" => PolicyKind::Raw,
        "clean_tool_repair" => PolicyKind::CleanToolRepair,
        "squash_failed_calls" => PolicyKind::SquashFailedCalls,
        "summary_prefix" => PolicyKind::SummaryPrefix,
        "reachability_gc" | "context_gc" | "tool_result_gc" => PolicyKind::ReachabilityGc,
        "custom" => PolicyKind::Custom,
        other => {
            return Err(VmError::Runtime(format!(
                "transcript_project: unknown policy '{other}' (expected raw, clean_tool_repair, squash_failed_calls, summary_prefix, reachability_gc, custom)"
            )))
        }
    };
    let respect_signatures = dict
        .as_ref()
        .and_then(|d| d.get("respect_provider_signatures"))
        .and_then(|v| match v {
            VmValue::Bool(b) => Some(*b),
            _ => None,
        })
        .unwrap_or(true);
    let reason = dict
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(|v| match v {
            VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
            _ => None,
        });
    let summary_keep_last = dict
        .as_ref()
        .and_then(|d| d.get("keep_last"))
        .and_then(|v| v.as_int())
        .filter(|n| *n >= 0)
        .map(|n| n as usize)
        .unwrap_or(0);
    let summary_text = dict
        .as_ref()
        .and_then(|d| d.get("summary"))
        .and_then(|v| match v {
            VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
            _ => None,
        });
    let custom = dict
        .as_ref()
        .and_then(|d| d.get("projector").or_else(|| d.get("custom")))
        .cloned()
        .filter(|v| matches!(v, VmValue::Closure(_)));
    if kind == PolicyKind::Custom && custom.is_none() {
        return Err(VmError::Runtime(
            "transcript_project: policy 'custom' requires a projector closure".into(),
        ));
    }
    let (
        gc_root_window,
        gc_min_chars,
        gc_root_labels,
        gc_root_texts,
        gc_require_write_barrier,
        gc_has_write_barrier,
    ) = if kind == PolicyKind::ReachabilityGc {
        let gc_root_window = optional_usize_alias(
            dict.as_ref(),
            &["root_window", "recent_messages", "keep_last"],
            DEFAULT_REACHABILITY_GC_ROOT_WINDOW,
            "transcript_project",
        )?;
        let gc_min_chars = optional_usize_alias(
            dict.as_ref(),
            &["min_chars", "min_reclaim_chars", "tool_result_min_chars"],
            DEFAULT_REACHABILITY_GC_MIN_CHARS,
            "transcript_project",
        )?;
        let gc_require_write_barrier = optional_bool_alias(
            dict.as_ref(),
            &["require_write_barrier", "require_barrier"],
            false,
            "transcript_project",
        )?;
        let (gc_root_labels, gc_root_texts, gc_has_write_barrier) =
            parse_reachability_gc_roots(dict.as_ref());
        (
            gc_root_window,
            gc_min_chars,
            gc_root_labels,
            gc_root_texts,
            gc_require_write_barrier,
            gc_has_write_barrier,
        )
    } else {
        (
            DEFAULT_REACHABILITY_GC_ROOT_WINDOW,
            DEFAULT_REACHABILITY_GC_MIN_CHARS,
            Vec::new(),
            Vec::new(),
            false,
            false,
        )
    };
    Ok(ProjectionPolicy {
        kind,
        respect_provider_signatures: respect_signatures,
        reason,
        summary_keep_last,
        summary_text,
        custom,
        gc_root_window,
        gc_min_chars,
        gc_root_labels,
        gc_root_texts,
        gc_require_write_barrier,
        gc_has_write_barrier,
    })
}

fn optional_usize_alias(
    dict: Option<&crate::value::DictMap>,
    keys: &[&str],
    default: usize,
    builtin: &str,
) -> Result<usize, VmError> {
    let Some(dict) = dict else {
        return Ok(default);
    };
    for key in keys {
        match dict.get(*key) {
            None | Some(VmValue::Nil) => {}
            Some(value) => {
                let Some(number) = value.as_int() else {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: `{key}` must be a non-negative int, got {}",
                        value.type_name()
                    )));
                };
                if number < 0 {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: `{key}` must be a non-negative int"
                    )));
                }
                return Ok(number as usize);
            }
        }
    }
    Ok(default)
}

fn optional_bool_alias(
    dict: Option<&crate::value::DictMap>,
    keys: &[&str],
    default: bool,
    builtin: &str,
) -> Result<bool, VmError> {
    let Some(dict) = dict else {
        return Ok(default);
    };
    for key in keys {
        match dict.get(*key) {
            None | Some(VmValue::Nil) => {}
            Some(VmValue::Bool(value)) => return Ok(*value),
            Some(value) => {
                return Err(VmError::Runtime(format!(
                    "{builtin}: `{key}` must be a bool, got {}",
                    value.type_name()
                )))
            }
        }
    }
    Ok(default)
}

fn parse_reachability_gc_roots(
    dict: Option<&crate::value::DictMap>,
) -> (Vec<String>, Vec<String>, bool) {
    let Some(dict) = dict else {
        return (Vec::new(), Vec::new(), false);
    };
    let mut labels = Vec::new();
    let mut texts = Vec::new();
    let mut has_write_barrier = false;
    let root_fields = [
        ("roots", "roots", false),
        ("active_plan", "active_plan", false),
        ("scratchpad", "scratchpad", false),
        ("pending_tool_args", "pending_tool_args", false),
        ("unresolved_findings", "unresolved_findings", false),
        ("write_barrier", "write_barrier", true),
        ("write_barrier_refs", "write_barrier", true),
        ("barrier_refs", "write_barrier", true),
    ];
    for (key, label, is_barrier) in root_fields {
        let Some(value) = dict.get(key) else {
            continue;
        };
        let before = texts.len();
        collect_vm_strings(value, &mut texts);
        if texts.len() > before {
            push_unique(&mut labels, label.to_string());
            has_write_barrier |= is_barrier;
        }
    }
    (labels, texts, has_write_barrier)
}

fn collect_vm_strings(value: &VmValue, out: &mut Vec<String>) {
    match value {
        VmValue::String(text) if !text.trim().is_empty() => {
            out.push(text.to_string());
        }
        VmValue::String(_) => {}
        VmValue::List(items) => {
            for item in items.iter() {
                collect_vm_strings(item, out);
            }
        }
        VmValue::Dict(map) => {
            for value in map.values() {
                collect_vm_strings(value, out);
            }
        }
        _ => {}
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

/// Outcome of a single projection.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionResult {
    pub messages: Vec<JsonValue>,
    /// Exact source index for each projected message. Synthetic messages and
    /// custom projections without caller-supplied indices remain `None`.
    pub source_message_indices: Vec<Option<usize>>,
    /// Indices into the source `messages` list that survived (in order).
    pub kept_indices: Vec<usize>,
    /// Source indices that were hidden from the projected prefix.
    pub dropped_indices: Vec<usize>,
    /// Sha256-hex of the canonical JSON of `messages`.
    pub prefix_hash: String,
    /// Effective reason; derives from `policy.reason` or a built-in
    /// label.
    pub reason: String,
    /// When `respect_provider_signatures` blocked a drop, this is true
    /// and `messages` is the unchanged raw prefix.
    pub provider_safety_blocked: bool,
    /// Source indices whose tool-result bodies were replaced by audit pointers.
    pub redacted_indices: Vec<usize>,
    /// Approximate token count removed from the projected prefix.
    pub reclaimed_tokens: usize,
    /// Character count removed from the projected prefix.
    pub reclaimed_chars: usize,
    /// Root source labels consulted by reachability-aware projection.
    pub roots_consulted: Vec<String>,
    /// Host/audit pointers for each reclaimed tool result body.
    pub redaction_pointers: Vec<JsonValue>,
}

pub(crate) async fn project_transcript(
    ctx: Option<&AsyncBuiltinCtx>,
    transcript: &crate::value::DictMap,
    policy: &ProjectionPolicy,
) -> Result<ProjectionResult, VmError> {
    let raw_messages = transcript_message_list(transcript)?;
    let raw_json: Vec<JsonValue> = raw_messages.iter().map(vm_value_to_json).collect();
    project_messages(ctx, &raw_json, transcript, policy).await
}

pub(crate) async fn project_messages(
    ctx: Option<&AsyncBuiltinCtx>,
    raw: &[JsonValue],
    transcript: &crate::value::DictMap,
    policy: &ProjectionPolicy,
) -> Result<ProjectionResult, VmError> {
    let mut decision = match policy.kind {
        PolicyKind::Raw => project_raw(raw),
        PolicyKind::CleanToolRepair => project_clean_tool_repair(raw),
        PolicyKind::SquashFailedCalls => project_squash_failed_calls(raw),
        PolicyKind::SummaryPrefix => project_summary_prefix(
            raw,
            transcript,
            policy.summary_keep_last,
            &policy.summary_text,
        ),
        PolicyKind::ReachabilityGc => reachability_gc::project(raw, policy),
        PolicyKind::Custom => {
            project_custom(ctx, raw, policy.custom.as_ref().expect("custom validated")).await?
        }
    };

    let mut safety_blocked = false;
    if policy.respect_provider_signatures && policy.kind != PolicyKind::Raw {
        let rewritten_indices = decision
            .dropped_indices
            .iter()
            .chain(decision.redacted_indices.iter())
            .copied()
            .collect::<Vec<_>>();
        if let Some(blocked_idx) = first_blocked_signed_drop(raw, &rewritten_indices) {
            safety_blocked = true;
            decision.reason = format!(
                "{} (blocked: signed reasoning at index {})",
                decision.reason, blocked_idx
            );
            decision.dropped_indices.clear();
            decision.redacted_indices.clear();
            decision.reclaimed_tokens = 0;
            decision.reclaimed_chars = 0;
            decision.redaction_pointers.clear();
            decision.kept_indices = (0..raw.len()).collect();
            decision.messages = raw.to_vec();
        }
    }

    let prefix_hash = hash_messages(&decision.messages);
    let source_message_indices = projected_source_indices(&decision, &policy.kind);
    let reason = policy
        .reason
        .clone()
        .filter(|_| !safety_blocked)
        .unwrap_or(decision.reason);

    Ok(ProjectionResult {
        source_message_indices,
        messages: decision.messages,
        kept_indices: decision.kept_indices,
        dropped_indices: decision.dropped_indices,
        prefix_hash,
        reason,
        provider_safety_blocked: safety_blocked,
        redacted_indices: decision.redacted_indices,
        reclaimed_tokens: decision.reclaimed_tokens,
        reclaimed_chars: decision.reclaimed_chars,
        roots_consulted: decision.root_labels,
        redaction_pointers: decision.redaction_pointers,
    })
}

#[derive(Debug)]
struct ProjectionDecision {
    messages: Vec<JsonValue>,
    kept_indices: Vec<usize>,
    dropped_indices: Vec<usize>,
    redacted_indices: Vec<usize>,
    reclaimed_tokens: usize,
    reclaimed_chars: usize,
    redaction_pointers: Vec<JsonValue>,
    root_labels: Vec<String>,
    reason: String,
    source_indices_are_exact: bool,
}

fn projected_source_indices(
    decision: &ProjectionDecision,
    policy: &PolicyKind,
) -> Vec<Option<usize>> {
    if !decision.source_indices_are_exact {
        return vec![None; decision.messages.len()];
    }
    if policy == &PolicyKind::SummaryPrefix
        && decision.messages.len() == decision.kept_indices.len() + 1
    {
        return std::iter::once(None)
            .chain(decision.kept_indices.iter().copied().map(Some))
            .collect();
    }
    if decision.messages.len() == decision.kept_indices.len() {
        return decision.kept_indices.iter().copied().map(Some).collect();
    }
    vec![None; decision.messages.len()]
}

fn project_raw(raw: &[JsonValue]) -> ProjectionDecision {
    ProjectionDecision {
        messages: raw.to_vec(),
        kept_indices: (0..raw.len()).collect(),
        dropped_indices: Vec::new(),
        redacted_indices: Vec::new(),
        reclaimed_tokens: 0,
        reclaimed_chars: 0,
        redaction_pointers: Vec::new(),
        root_labels: Vec::new(),
        reason: "raw_passthrough".to_string(),
        source_indices_are_exact: true,
    }
}

/// `clean_tool_repair`: hide the *last* failed tool_call + its
/// observation message in a (failed, observation, retry, success) chain.
/// Only the most-recent retry survives. If a tool call by name fails
/// once and then the same tool name succeeds in a later assistant turn,
/// the failed pair is removed from the projected prefix.
fn project_clean_tool_repair(raw: &[JsonValue]) -> ProjectionDecision {
    let mut dropped: Vec<usize> = Vec::new();
    // Map tool_name -> indices of failed assistant turns calling it, and
    // their corresponding tool_result indices.
    let mut failed_for_tool: BTreeMap<String, Vec<FailedCallRecord>> =
        std::collections::BTreeMap::new();

    for (idx, msg) in raw.iter().enumerate() {
        if msg.get("role").and_then(JsonValue::as_str) != Some("assistant") {
            continue;
        }
        let tool_calls = extract_tool_calls(msg);
        for call in &tool_calls {
            let Some(tool_name) = &call.tool_name else {
                continue;
            };
            // Look ahead for tool_results for this call id.
            let mut error_result_idx: Option<usize> = None;
            for (offset, follow) in raw[idx + 1..].iter().enumerate() {
                let follow_idx = idx + 1 + offset;
                let follow_role = follow.get("role").and_then(JsonValue::as_str).unwrap_or("");
                if !matches!(follow_role, "tool" | "tool_result") {
                    if follow_role == "assistant" {
                        break;
                    }
                    continue;
                }
                if tool_result_matches(follow, call) {
                    if tool_result_is_error(follow) {
                        error_result_idx = Some(follow_idx);
                    }
                    break;
                }
            }
            if let Some(result_idx) = error_result_idx {
                failed_for_tool
                    .entry(tool_name.clone())
                    .or_default()
                    .push(FailedCallRecord {
                        assistant_idx: idx,
                        result_idx,
                    });
            }
        }
    }

    // Second pass: a tool that later succeeded means we can drop earlier
    // failed pairs for that tool.
    for (tool_name, failures) in failed_for_tool.iter() {
        let mut later_success = false;
        for (i, msg) in raw.iter().enumerate() {
            if msg.get("role").and_then(JsonValue::as_str) != Some("assistant") {
                continue;
            }
            let calls = extract_tool_calls(msg);
            for call in &calls {
                if call.tool_name.as_deref() != Some(tool_name.as_str()) {
                    continue;
                }
                if let Some(result_idx) = find_tool_result_idx(raw, i, call) {
                    if !tool_result_is_error(&raw[result_idx]) {
                        later_success = true;
                    }
                }
            }
            if later_success {
                break;
            }
        }
        if later_success {
            for failure in failures {
                dropped.push(failure.assistant_idx);
                dropped.push(failure.result_idx);
            }
        }
    }

    dropped.sort_unstable();
    dropped.dedup();
    project_with_drops(raw, &dropped, "clean_tool_repair")
}

/// `squash_failed_calls`: every assistant message whose only outcome was
/// a failed tool call (and its observation) is removed from the
/// projected prefix. Assistant messages mixing failures with surviving
/// prose/calls remain — we keep them and only drop the matched
/// tool_result.
fn project_squash_failed_calls(raw: &[JsonValue]) -> ProjectionDecision {
    let mut dropped: Vec<usize> = Vec::new();
    for (idx, msg) in raw.iter().enumerate() {
        if msg.get("role").and_then(JsonValue::as_str) != Some("assistant") {
            continue;
        }
        let calls = extract_tool_calls(msg);
        if calls.is_empty() {
            continue;
        }
        let mut all_failed = true;
        let mut failed_result_indices = Vec::new();
        for call in &calls {
            if let Some(result_idx) = find_tool_result_idx(raw, idx, call) {
                if tool_result_is_error(&raw[result_idx]) {
                    failed_result_indices.push(result_idx);
                } else {
                    all_failed = false;
                }
            } else {
                all_failed = false;
            }
        }
        if all_failed && !failed_result_indices.is_empty() && text_is_empty(msg) {
            dropped.push(idx);
        }
        dropped.extend(failed_result_indices);
    }
    dropped.sort_unstable();
    dropped.dedup();
    project_with_drops(raw, &dropped, "squash_failed_calls")
}

/// `summary_prefix`: keep the final `keep_last` messages verbatim;
/// replace earlier history with one synthetic system message containing
/// `summary_text` (falling back to `transcript.summary` or a generated
/// "[N earlier messages summarized]" placeholder). The synthetic
/// summary message is *not* a raw event — its purpose is purely to give
/// the next provider call a compact roll-up.
fn project_summary_prefix(
    raw: &[JsonValue],
    transcript: &crate::value::DictMap,
    keep_last: usize,
    summary_text: &Option<String>,
) -> ProjectionDecision {
    if keep_last >= raw.len() {
        let mut decision = project_raw(raw);
        decision.reason = "summary_prefix_noop_short_history".to_string();
        return decision;
    }
    let drop_count = raw.len() - keep_last;
    let dropped: Vec<usize> = (0..drop_count).collect();
    let kept: Vec<usize> = (drop_count..raw.len()).collect();
    let summary_body = summary_text
        .clone()
        .or_else(|| {
            transcript.get("summary").and_then(|v| match v {
                VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            })
        })
        .unwrap_or_else(|| format!("[{drop_count} earlier messages summarized]"));
    let mut messages = Vec::with_capacity(kept.len() + 1);
    messages.push(serde_json::json!({
        "role": "system",
        "content": summary_body,
        "_harn_projection": {
            "synthetic": true,
            "policy": "summary_prefix",
        },
    }));
    for kept_idx in &kept {
        messages.push(raw[*kept_idx].clone());
    }
    ProjectionDecision {
        messages,
        kept_indices: kept,
        dropped_indices: dropped,
        redacted_indices: Vec::new(),
        reclaimed_tokens: 0,
        reclaimed_chars: 0,
        redaction_pointers: Vec::new(),
        root_labels: Vec::new(),
        reason: "summary_prefix".to_string(),
        source_indices_are_exact: true,
    }
}

async fn project_custom(
    ctx: Option<&AsyncBuiltinCtx>,
    raw: &[JsonValue],
    callback: &VmValue,
) -> Result<ProjectionDecision, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "transcript_project: custom projector must be a closure".into(),
        ));
    };
    let mut vm = ctx.map(AsyncBuiltinCtx::child_vm).ok_or_else(|| {
        VmError::Runtime("transcript_project: custom projector requires an async VM context".into())
    })?;
    let raw_vm = VmValue::List(std::sync::Arc::new(
        raw.iter().map(json_to_vm_value).collect(),
    ));
    let result = vm.call_closure_pub(&closure, &[raw_vm]).await?;
    parse_custom_projector_result(raw, &result)
}

fn parse_custom_projector_result(
    raw: &[JsonValue],
    value: &VmValue,
) -> Result<ProjectionDecision, VmError> {
    match value {
        VmValue::List(items) => {
            let messages: Vec<JsonValue> = items.iter().map(vm_value_to_json).collect();
            let kept_indices = derive_kept_indices(raw, &messages);
            let dropped_indices = derive_dropped_indices(raw.len(), &kept_indices);
            Ok(ProjectionDecision {
                messages,
                kept_indices,
                dropped_indices,
                redacted_indices: Vec::new(),
                reclaimed_tokens: 0,
                reclaimed_chars: 0,
                redaction_pointers: Vec::new(),
                root_labels: Vec::new(),
                reason: "custom".to_string(),
                source_indices_are_exact: false,
            })
        }
        VmValue::Dict(dict) => {
            let messages_value = dict.get("messages").cloned().ok_or_else(|| {
                VmError::Runtime(
                    "transcript_project: custom projector must return a list or a dict with `messages`".into(),
                )
            })?;
            let messages: Vec<JsonValue> = match messages_value {
                VmValue::List(items) => items.iter().map(vm_value_to_json).collect(),
                _ => {
                    return Err(VmError::Runtime(
                        "transcript_project: custom projector `messages` must be a list".into(),
                    ))
                }
            };
            let reason = dict
                .get("reason")
                .and_then(|v| match v {
                    VmValue::String(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "custom".to_string());
            let exact_kept_indices =
                validated_exact_kept_indices(dict.get("kept_indices"), raw.len(), messages.len());
            let source_indices_are_exact = exact_kept_indices.is_some();
            let kept_indices =
                exact_kept_indices.unwrap_or_else(|| derive_kept_indices(raw, &messages));
            let dropped_indices = dict
                .get("dropped_indices")
                .and_then(|v| match v {
                    VmValue::List(items) => Some(
                        items
                            .iter()
                            .filter_map(|item| item.as_int().map(|n| n.max(0) as usize))
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .unwrap_or_else(|| derive_dropped_indices(raw.len(), &kept_indices));
            Ok(ProjectionDecision {
                messages,
                kept_indices,
                dropped_indices,
                redacted_indices: Vec::new(),
                reclaimed_tokens: 0,
                reclaimed_chars: 0,
                redaction_pointers: Vec::new(),
                root_labels: Vec::new(),
                reason,
                source_indices_are_exact,
            })
        }
        _ => Err(VmError::Runtime(
            "transcript_project: custom projector must return a list or dict".into(),
        )),
    }
}

fn validated_exact_kept_indices(
    value: Option<&VmValue>,
    source_len: usize,
    projected_len: usize,
) -> Option<Vec<usize>> {
    let VmValue::List(items) = value? else {
        return None;
    };
    let indices = items
        .iter()
        .map(|item| usize::try_from(item.as_int()?).ok())
        .collect::<Option<Vec<_>>>()?;
    if indices.len() != projected_len
        || indices.iter().any(|index| *index >= source_len)
        || indices.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return None;
    }
    Some(indices)
}

/// Best-effort recovery of the kept index map for custom projectors:
/// match each output message back to the first un-claimed raw message
/// whose JSON serializes identically. Synthetic messages that don't
/// match are skipped silently.
fn derive_kept_indices(raw: &[JsonValue], projected: &[JsonValue]) -> Vec<usize> {
    let mut next_raw = 0;
    let mut kept = Vec::with_capacity(projected.len());
    for msg in projected {
        let mut hit: Option<usize> = None;
        for (offset, candidate) in raw.iter().enumerate().skip(next_raw) {
            if candidate == msg {
                hit = Some(offset);
                break;
            }
        }
        if let Some(idx) = hit {
            kept.push(idx);
            next_raw = idx + 1;
        }
    }
    kept
}

fn derive_dropped_indices(raw_len: usize, kept: &[usize]) -> Vec<usize> {
    let kept_set: std::collections::HashSet<usize> = kept.iter().copied().collect();
    (0..raw_len).filter(|i| !kept_set.contains(i)).collect()
}

fn project_with_drops(raw: &[JsonValue], dropped: &[usize], reason: &str) -> ProjectionDecision {
    let drop_set: std::collections::HashSet<usize> = dropped.iter().copied().collect();
    let mut kept = Vec::with_capacity(raw.len());
    let mut messages = Vec::with_capacity(raw.len());
    for (idx, msg) in raw.iter().enumerate() {
        if drop_set.contains(&idx) {
            continue;
        }
        kept.push(idx);
        messages.push(msg.clone());
    }
    ProjectionDecision {
        messages,
        kept_indices: kept,
        dropped_indices: dropped.to_vec(),
        redacted_indices: Vec::new(),
        reclaimed_tokens: 0,
        reclaimed_chars: 0,
        redaction_pointers: Vec::new(),
        root_labels: Vec::new(),
        reason: reason.to_string(),
        source_indices_are_exact: true,
    }
}

#[derive(Clone, Debug)]
struct ToolCallInfo {
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    node: Option<JsonValue>,
}

#[derive(Clone, Debug)]
struct ToolResultCandidate {
    message_idx: usize,
    block_idx: Option<usize>,
    content: String,
    node: JsonValue,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    is_error: bool,
}

#[derive(Clone, Debug)]
struct FailedCallRecord {
    assistant_idx: usize,
    result_idx: usize,
}

fn extract_tool_calls(message: &JsonValue) -> Vec<ToolCallInfo> {
    let mut calls = Vec::new();
    if let Some(items) = message.get("tool_calls").and_then(JsonValue::as_array) {
        for item in items {
            calls.push(ToolCallInfo {
                tool_call_id: extract_tool_call_id(item),
                tool_name: extract_tool_call_name(item),
                node: Some(item.clone()),
            });
        }
    }
    if let Some(items) = message.get("content").and_then(JsonValue::as_array) {
        for item in items {
            if item.get("type").and_then(JsonValue::as_str) == Some("tool_use") {
                calls.push(ToolCallInfo {
                    tool_call_id: item
                        .get("id")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                    tool_name: item
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                    node: Some(item.clone()),
                });
            }
        }
    }
    calls
}

fn extract_tool_call_id(call: &JsonValue) -> Option<String> {
    call.get("id")
        .or_else(|| call.get("tool_call_id"))
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn extract_tool_call_name(call: &JsonValue) -> Option<String> {
    let direct = call
        .get("name")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    if direct.is_some() {
        return direct;
    }
    call.get("function")
        .and_then(|f| f.get("name"))
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn tool_result_matches(message: &JsonValue, call: &ToolCallInfo) -> bool {
    let call_id = call.tool_call_id.as_deref();
    let target_id = message
        .get("tool_use_id")
        .or_else(|| message.get("tool_call_id"))
        .and_then(JsonValue::as_str);
    if let (Some(call_id), Some(target_id)) = (call_id, target_id) {
        return call_id == target_id;
    }
    if message.get("role").and_then(JsonValue::as_str) == Some("user") {
        if let Some(blocks) = message.get("content").and_then(JsonValue::as_array) {
            for block in blocks {
                if block.get("type").and_then(JsonValue::as_str) != Some("tool_result") {
                    continue;
                }
                let target_id = block
                    .get("tool_use_id")
                    .or_else(|| block.get("tool_call_id"))
                    .or_else(|| block.get("id"))
                    .and_then(JsonValue::as_str);
                if let (Some(call_id), Some(target_id)) = (call_id, target_id) {
                    return call_id == target_id;
                }
                let target_name = block.get("name").and_then(JsonValue::as_str);
                if let (Some(name), Some(target_name)) = (call.tool_name.as_deref(), target_name) {
                    return name == target_name;
                }
            }
        }
    }
    let tool_name = call.tool_name.as_deref();
    let message_name = message.get("name").and_then(JsonValue::as_str);
    if let (Some(name), Some(msg_name)) = (tool_name, message_name) {
        return name == msg_name;
    }
    false
}

fn tool_result_is_error(message: &JsonValue) -> bool {
    if let Some(candidate) = tool_result_candidates(0, message).into_iter().next() {
        return candidate.is_error;
    }
    tool_result_node_is_error(message)
}

fn tool_result_candidates(message_idx: usize, message: &JsonValue) -> Vec<ToolResultCandidate> {
    let role = message
        .get("role")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if matches!(role, "tool" | "tool_result") {
        return message
            .get("content")
            .map(json_text)
            .map(|content| {
                vec![ToolResultCandidate {
                    message_idx,
                    block_idx: None,
                    content,
                    node: message.clone(),
                    tool_call_id: message
                        .get("tool_use_id")
                        .or_else(|| message.get("tool_call_id"))
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                    tool_name: message
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                    is_error: tool_result_node_is_error(message),
                }]
            })
            .unwrap_or_default();
    }
    if role != "user" {
        return Vec::new();
    }
    let Some(blocks) = message.get("content").and_then(JsonValue::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .enumerate()
        .filter_map(|(block_idx, block)| {
            if block.get("type").and_then(JsonValue::as_str) == Some("tool_result") {
                return block
                    .get("content")
                    .map(json_text)
                    .map(|content| ToolResultCandidate {
                        message_idx,
                        block_idx: Some(block_idx),
                        content,
                        node: block.clone(),
                        tool_call_id: block
                            .get("tool_use_id")
                            .or_else(|| block.get("tool_call_id"))
                            .or_else(|| block.get("id"))
                            .and_then(JsonValue::as_str)
                            .map(str::to_string),
                        tool_name: block
                            .get("name")
                            .or_else(|| message.get("name"))
                            .and_then(JsonValue::as_str)
                            .map(str::to_string),
                        is_error: tool_result_node_is_error(block),
                    });
            }
            None
        })
        .collect()
}

fn tool_result_node_is_error(node: &JsonValue) -> bool {
    if node
        .get("is_error")
        .or_else(|| node.get("error"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(status) = node.get("status").and_then(JsonValue::as_str) {
        if matches!(
            status,
            "error" | "failed" | "rejected" | "exception" | "denied"
        ) {
            return true;
        }
    }
    let content = node.get("content").map(json_text).unwrap_or_default();
    let lowered = content.trim_start().to_ascii_lowercase();
    lowered.starts_with("error:")
        || lowered.starts_with("tool error")
        || lowered.starts_with("failed:")
        || lowered.contains("\"is_error\":true")
}

fn json_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn find_tool_result_idx(
    raw: &[JsonValue],
    assistant_idx: usize,
    call: &ToolCallInfo,
) -> Option<usize> {
    for (offset, follow) in raw[assistant_idx + 1..].iter().enumerate() {
        let role = follow.get("role").and_then(JsonValue::as_str).unwrap_or("");
        if matches!(role, "tool" | "tool_result") && tool_result_matches(follow, call) {
            return Some(assistant_idx + 1 + offset);
        }
        if role == "assistant" {
            break;
        }
    }
    None
}

fn text_is_empty(message: &JsonValue) -> bool {
    let content = message.get("content");
    match content {
        Some(JsonValue::String(s)) => s.trim().is_empty(),
        Some(JsonValue::Array(items)) => items.iter().all(|item| {
            let kind = item.get("type").and_then(JsonValue::as_str).unwrap_or("");
            match kind {
                "text" | "output_text" => item
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true),
                _ => true,
            }
        }),
        _ => true,
    }
}

/// A signed thinking block (Anthropic) carries an opaque signature
/// proving the block hasn't been tampered with. Removing such a message
/// from the prefix would invalidate the provider contract on the next
/// turn, so projection refuses unless the host opts out.
fn first_blocked_signed_drop(raw: &[JsonValue], dropped: &[usize]) -> Option<usize> {
    for idx in dropped {
        let Some(msg) = raw.get(*idx) else { continue };
        if message_has_signed_reasoning(msg) {
            return Some(*idx);
        }
    }
    None
}

fn message_has_signed_reasoning(message: &JsonValue) -> bool {
    if let Some(items) = message.get("content").and_then(JsonValue::as_array) {
        for block in items {
            let kind = block.get("type").and_then(JsonValue::as_str).unwrap_or("");
            if !matches!(kind, "thinking" | "redacted_thinking" | "reasoning") {
                continue;
            }
            if block.get("signature").is_some() || kind == "redacted_thinking" {
                return true;
            }
        }
    }
    if message.get("thinking_signature").is_some() {
        return true;
    }
    false
}

fn hash_messages(messages: &[JsonValue]) -> String {
    let canonical = canonical_json::to_string(&JsonValue::Array(messages.to_vec()));
    hash_string(&canonical)
}

fn hash_string(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    format!("sha256:{}", hex::encode(digest))
}

pub(crate) fn result_to_vm(result: &ProjectionResult, policy: &ProjectionPolicy) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("policy", policy.kind.as_str());
    dict.put_str("reason", result.reason.clone());
    dict.put_str("prefix_hash", result.prefix_hash.clone());
    dict.insert(
        crate::value::intern_key("messages"),
        VmValue::List(std::sync::Arc::new(
            result.messages.iter().map(json_to_vm_value).collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("kept_indices"),
        VmValue::List(std::sync::Arc::new(
            result
                .kept_indices
                .iter()
                .map(|i| VmValue::Int(*i as i64))
                .collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("dropped_indices"),
        VmValue::List(std::sync::Arc::new(
            result
                .dropped_indices
                .iter()
                .map(|i| VmValue::Int(*i as i64))
                .collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("kept_count"),
        VmValue::Int(result.kept_indices.len() as i64),
    );
    dict.insert(
        crate::value::intern_key("dropped_count"),
        VmValue::Int(result.dropped_indices.len() as i64),
    );
    dict.insert(
        crate::value::intern_key("redacted_indices"),
        VmValue::List(std::sync::Arc::new(
            result
                .redacted_indices
                .iter()
                .map(|i| VmValue::Int(*i as i64))
                .collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("redacted_count"),
        VmValue::Int(result.redaction_pointers.len() as i64),
    );
    dict.insert(
        crate::value::intern_key("reclaimed_tokens"),
        VmValue::Int(result.reclaimed_tokens as i64),
    );
    dict.insert(
        crate::value::intern_key("reclaimed_chars"),
        VmValue::Int(result.reclaimed_chars as i64),
    );
    dict.insert(
        crate::value::intern_key("roots_consulted"),
        VmValue::List(std::sync::Arc::new(
            result
                .roots_consulted
                .iter()
                .map(|label| VmValue::String(arcstr::ArcStr::from(label.clone())))
                .collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("redaction_pointers"),
        VmValue::List(std::sync::Arc::new(
            result
                .redaction_pointers
                .iter()
                .map(json_to_vm_value)
                .collect(),
        )),
    );
    dict.insert(
        crate::value::intern_key("provider_safety_blocked"),
        VmValue::Bool(result.provider_safety_blocked),
    );
    dict.insert(
        crate::value::intern_key("event"),
        projection_event_value(result, policy),
    );
    VmValue::dict(dict)
}

pub(crate) fn projection_event_value(
    result: &ProjectionResult,
    policy: &ProjectionPolicy,
) -> VmValue {
    let metadata = projection_event_metadata(result, policy);
    transcript_event(
        TRANSCRIPT_PROJECTION_EVENT_KIND,
        "system",
        "internal",
        &result.reason,
        Some(metadata),
    )
}

fn projection_event_metadata(result: &ProjectionResult, policy: &ProjectionPolicy) -> JsonValue {
    serde_json::json!({
        "policy": policy.kind.as_str(),
        "reason": result.reason,
        "prefix_hash": result.prefix_hash,
        "kept_indices": result.kept_indices,
        "dropped_indices": result.dropped_indices,
        "kept_count": result.kept_indices.len(),
        "dropped_count": result.dropped_indices.len(),
        "redacted_indices": result.redacted_indices,
        "redacted_count": result.redaction_pointers.len(),
        "reclaimed_tokens": result.reclaimed_tokens,
        "reclaimed_chars": result.reclaimed_chars,
        "roots_consulted": result.roots_consulted,
        "redaction_pointers": result.redaction_pointers,
        "respects_provider_signatures": policy.respect_provider_signatures,
        "provider_safety_blocked": result.provider_safety_blocked,
        "summary_keep_last": match policy.kind {
            PolicyKind::SummaryPrefix => Some(policy.summary_keep_last),
            _ => None,
        },
    })
}

#[cfg(test)]
#[path = "transcript_project_tests.rs"]
mod tests;
