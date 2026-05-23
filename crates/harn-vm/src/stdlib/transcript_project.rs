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

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::llm::helpers::{
    is_transcript_value, transcript_event, transcript_message_list, vm_value_to_json,
};
use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Canonical `kind` for the transcript event emitted on each projection.
pub(crate) const TRANSCRIPT_PROJECTION_EVENT_KIND: &str = "transcript.projection";

pub(crate) fn register_transcript_projection_builtins(vm: &mut Vm) {
    vm.register_async_builtin("transcript_project", |args| async move {
        let transcript_value = args.first().cloned().unwrap_or(VmValue::Nil);
        let transcript = transcript_value
            .as_dict()
            .filter(|_| is_transcript_value(&transcript_value))
            .ok_or_else(|| {
                VmError::Runtime("transcript_project: first argument must be a transcript".into())
            })?;
        let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let policy = parse_projection_options(&options)?;
        let result = project_transcript(transcript, &policy).await?;
        Ok(result_to_vm(&result, &policy))
    });
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyKind {
    Raw,
    CleanToolRepair,
    SquashFailedCalls,
    SummaryPrefix,
    Custom,
}

impl PolicyKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            PolicyKind::Raw => "raw",
            PolicyKind::CleanToolRepair => "clean_tool_repair",
            PolicyKind::SquashFailedCalls => "squash_failed_calls",
            PolicyKind::SummaryPrefix => "summary_prefix",
            PolicyKind::Custom => "custom",
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
        "custom" => PolicyKind::Custom,
        other => {
            return Err(VmError::Runtime(format!(
                "transcript_project: unknown policy '{other}' (expected raw, clean_tool_repair, squash_failed_calls, summary_prefix, custom)"
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
    Ok(ProjectionPolicy {
        kind,
        respect_provider_signatures: respect_signatures,
        reason,
        summary_keep_last,
        summary_text,
        custom,
    })
}

/// Outcome of a single projection.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionResult {
    pub messages: Vec<JsonValue>,
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
}

pub(crate) async fn project_transcript(
    transcript: &BTreeMap<String, VmValue>,
    policy: &ProjectionPolicy,
) -> Result<ProjectionResult, VmError> {
    let raw_messages = transcript_message_list(transcript)?;
    let raw_json: Vec<JsonValue> = raw_messages.iter().map(vm_value_to_json).collect();
    project_messages(&raw_json, transcript, policy).await
}

pub(crate) async fn project_messages(
    raw: &[JsonValue],
    transcript: &BTreeMap<String, VmValue>,
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
        PolicyKind::Custom => {
            project_custom(raw, policy.custom.as_ref().expect("custom validated")).await?
        }
    };

    let mut safety_blocked = false;
    if policy.respect_provider_signatures && policy.kind != PolicyKind::Raw {
        if let Some(blocked_idx) = first_blocked_signed_drop(raw, &decision.dropped_indices) {
            safety_blocked = true;
            decision.reason = format!(
                "{} (blocked: signed reasoning at index {})",
                decision.reason, blocked_idx
            );
            decision.dropped_indices.clear();
            decision.kept_indices = (0..raw.len()).collect();
            decision.messages = raw.to_vec();
        }
    }

    let prefix_hash = hash_messages(&decision.messages);
    let reason = policy
        .reason
        .clone()
        .filter(|_| !safety_blocked)
        .unwrap_or(decision.reason);

    Ok(ProjectionResult {
        messages: decision.messages,
        kept_indices: decision.kept_indices,
        dropped_indices: decision.dropped_indices,
        prefix_hash,
        reason,
        provider_safety_blocked: safety_blocked,
    })
}

#[derive(Debug)]
struct ProjectionDecision {
    messages: Vec<JsonValue>,
    kept_indices: Vec<usize>,
    dropped_indices: Vec<usize>,
    reason: String,
}

fn project_raw(raw: &[JsonValue]) -> ProjectionDecision {
    ProjectionDecision {
        messages: raw.to_vec(),
        kept_indices: (0..raw.len()).collect(),
        dropped_indices: Vec::new(),
        reason: "raw_passthrough".to_string(),
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
    let mut failed_for_tool: BTreeMap<String, Vec<FailedCallRecord>> = BTreeMap::new();

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
    transcript: &BTreeMap<String, VmValue>,
    keep_last: usize,
    summary_text: &Option<String>,
) -> ProjectionDecision {
    if keep_last >= raw.len() {
        return ProjectionDecision {
            messages: raw.to_vec(),
            kept_indices: (0..raw.len()).collect(),
            dropped_indices: Vec::new(),
            reason: "summary_prefix_noop_short_history".to_string(),
        };
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
        reason: "summary_prefix".to_string(),
    }
}

async fn project_custom(
    raw: &[JsonValue],
    callback: &VmValue,
) -> Result<ProjectionDecision, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "transcript_project: custom projector must be a closure".into(),
        ));
    };
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("transcript_project: custom projector requires an async VM context".into())
    })?;
    let raw_vm = VmValue::List(Rc::new(raw.iter().map(json_to_vm_value).collect()));
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
                reason: "custom".to_string(),
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
            let kept_indices = dict
                .get("kept_indices")
                .and_then(|v| match v {
                    VmValue::List(items) => Some(
                        items
                            .iter()
                            .filter_map(|item| item.as_int().map(|n| n.max(0) as usize))
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .unwrap_or_else(|| derive_kept_indices(raw, &messages));
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
                reason,
            })
        }
        _ => Err(VmError::Runtime(
            "transcript_project: custom projector must return a list or dict".into(),
        )),
    }
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
        reason: reason.to_string(),
    }
}

#[derive(Clone, Debug)]
struct ToolCallInfo {
    tool_call_id: Option<String>,
    tool_name: Option<String>,
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
    let tool_name = call.tool_name.as_deref();
    let message_name = message.get("name").and_then(JsonValue::as_str);
    if let (Some(name), Some(msg_name)) = (tool_name, message_name) {
        return name == msg_name;
    }
    false
}

fn tool_result_is_error(message: &JsonValue) -> bool {
    if message
        .get("is_error")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(status) = message.get("status").and_then(JsonValue::as_str) {
        if matches!(
            status,
            "error" | "failed" | "rejected" | "exception" | "denied"
        ) {
            return true;
        }
    }
    let content = message
        .get("content")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let lowered = content.trim_start().to_ascii_lowercase();
    lowered.starts_with("error:")
        || lowered.starts_with("tool error")
        || lowered.starts_with("failed:")
        || lowered.contains("\"is_error\":true")
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
    let canonical = canonical_json(&JsonValue::Array(messages.to_vec()));
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    format!("sha256:{}", hex::encode(digest))
}

fn canonical_json(value: &JsonValue) -> String {
    match value {
        JsonValue::Object(map) => {
            let mut parts = Vec::with_capacity(map.len());
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                let escaped_key = serde_json::to_string(key).unwrap_or_default();
                parts.push(format!("{}:{}", escaped_key, canonical_json(&map[key])));
            }
            format!("{{{}}}", parts.join(","))
        }
        JsonValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
    }
}

pub(crate) fn result_to_vm(result: &ProjectionResult, policy: &ProjectionPolicy) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "policy".to_string(),
        VmValue::String(Rc::from(policy.kind.as_str())),
    );
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(result.reason.clone())),
    );
    dict.insert(
        "prefix_hash".to_string(),
        VmValue::String(Rc::from(result.prefix_hash.clone())),
    );
    dict.insert(
        "messages".to_string(),
        VmValue::List(Rc::new(
            result.messages.iter().map(json_to_vm_value).collect(),
        )),
    );
    dict.insert(
        "kept_indices".to_string(),
        VmValue::List(Rc::new(
            result
                .kept_indices
                .iter()
                .map(|i| VmValue::Int(*i as i64))
                .collect(),
        )),
    );
    dict.insert(
        "dropped_indices".to_string(),
        VmValue::List(Rc::new(
            result
                .dropped_indices
                .iter()
                .map(|i| VmValue::Int(*i as i64))
                .collect(),
        )),
    );
    dict.insert(
        "kept_count".to_string(),
        VmValue::Int(result.kept_indices.len() as i64),
    );
    dict.insert(
        "dropped_count".to_string(),
        VmValue::Int(result.dropped_indices.len() as i64),
    );
    dict.insert(
        "provider_safety_blocked".to_string(),
        VmValue::Bool(result.provider_safety_blocked),
    );
    dict.insert("event".to_string(), projection_event_value(result, policy));
    VmValue::Dict(Rc::new(dict))
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
        "respects_provider_signatures": policy.respect_provider_signatures,
        "provider_safety_blocked": result.provider_safety_blocked,
        "summary_keep_last": match policy.kind {
            PolicyKind::SummaryPrefix => Some(policy.summary_keep_last),
            _ => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_dict_to_btree(value: &JsonValue) -> BTreeMap<String, VmValue> {
        match json_to_vm_value(value) {
            VmValue::Dict(d) => (*d).clone(),
            _ => BTreeMap::new(),
        }
    }

    fn transcript_with(
        messages: Vec<JsonValue>,
        summary: Option<&str>,
    ) -> BTreeMap<String, VmValue> {
        let mut transcript = serde_json::json!({
            "_type": "transcript",
            "version": 2,
            "messages": messages,
            "events": [],
            "assets": [],
        });
        if let Some(text) = summary {
            transcript["summary"] = serde_json::Value::String(text.to_string());
        }
        json_dict_to_btree(&transcript)
    }

    #[tokio::test]
    async fn raw_policy_is_identity_and_emits_hash() {
        let transcript = transcript_with(
            vec![
                serde_json::json!({"role": "user", "content": "hi"}),
                serde_json::json!({"role": "assistant", "content": "hey"}),
            ],
            None,
        );
        let policy = ProjectionPolicy {
            kind: PolicyKind::Raw,
            respect_provider_signatures: true,
            reason: None,
            summary_keep_last: 0,
            summary_text: None,
            custom: None,
        };
        let result = project_transcript(&transcript, &policy).await.unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.dropped_indices.len(), 0);
        assert!(result.prefix_hash.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn clean_tool_repair_drops_failed_then_success_pair() {
        let transcript = transcript_with(
            vec![
                serde_json::json!({"role": "user", "content": "run it"}),
                serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "call_1", "name": "run", "arguments": {}}],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "name": "run",
                    "content": "Error: missing arg",
                    "is_error": true,
                }),
                serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "call_2", "name": "run", "arguments": {"arg": "x"}}],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "call_2",
                    "name": "run",
                    "content": "ok",
                }),
            ],
            None,
        );
        let policy = ProjectionPolicy {
            kind: PolicyKind::CleanToolRepair,
            respect_provider_signatures: true,
            reason: None,
            summary_keep_last: 0,
            summary_text: None,
            custom: None,
        };
        let result = project_transcript(&transcript, &policy).await.unwrap();
        // Drops the failed assistant (index 1) and its error tool result (index 2).
        assert_eq!(result.dropped_indices, vec![1, 2]);
        assert_eq!(result.kept_indices, vec![0, 3, 4]);
        assert_eq!(result.messages.len(), 3);
        assert!(result
            .messages
            .last()
            .and_then(|m| m.get("content"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .contains("ok"));
    }

    #[tokio::test]
    async fn squash_failed_calls_drops_orphan_failures() {
        let transcript = transcript_with(
            vec![
                serde_json::json!({"role": "user", "content": "go"}),
                serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "a", "name": "lookup", "arguments": {}}],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "a",
                    "name": "lookup",
                    "content": "Error: not found",
                    "is_error": true,
                }),
                serde_json::json!({"role": "assistant", "content": "Done after all."}),
            ],
            None,
        );
        let policy = ProjectionPolicy {
            kind: PolicyKind::SquashFailedCalls,
            respect_provider_signatures: true,
            reason: None,
            summary_keep_last: 0,
            summary_text: None,
            custom: None,
        };
        let result = project_transcript(&transcript, &policy).await.unwrap();
        assert_eq!(result.dropped_indices, vec![1, 2]);
        assert_eq!(result.messages.len(), 2);
    }

    #[tokio::test]
    async fn summary_prefix_replaces_old_history_with_synthetic_message() {
        let transcript = transcript_with(
            (0..6)
                .map(|i| {
                    serde_json::json!({
                        "role": if i % 2 == 0 { "user" } else { "assistant" },
                        "content": format!("msg{}", i),
                    })
                })
                .collect(),
            Some("Earlier work boiled down."),
        );
        let policy = ProjectionPolicy {
            kind: PolicyKind::SummaryPrefix,
            respect_provider_signatures: true,
            reason: None,
            summary_keep_last: 2,
            summary_text: None,
            custom: None,
        };
        let result = project_transcript(&transcript, &policy).await.unwrap();
        assert_eq!(result.messages.len(), 3); // 1 summary + 2 kept
        assert_eq!(result.dropped_indices.len(), 4);
        assert_eq!(result.kept_indices, vec![4, 5]);
        let summary_msg = &result.messages[0];
        assert_eq!(
            summary_msg.get("role").and_then(JsonValue::as_str),
            Some("system")
        );
        assert_eq!(
            summary_msg.get("content").and_then(JsonValue::as_str),
            Some("Earlier work boiled down.")
        );
        assert!(summary_msg
            .get("_harn_projection")
            .and_then(|p| p.get("synthetic"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn provider_safety_blocks_dropping_signed_reasoning() {
        let transcript = transcript_with(
            vec![
                serde_json::json!({"role": "user", "content": "first"}),
                serde_json::json!({
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "private chain", "signature": "abc123"},
                        {"type": "tool_use", "id": "call_x", "name": "run", "input": {}}
                    ],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "call_x",
                    "name": "run",
                    "content": "Error: boom",
                    "is_error": true,
                }),
                serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "call_y", "name": "run", "arguments": {"v": 1}}],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "call_y",
                    "name": "run",
                    "content": "ok",
                }),
            ],
            None,
        );
        let policy = ProjectionPolicy {
            kind: PolicyKind::CleanToolRepair,
            respect_provider_signatures: true,
            reason: None,
            summary_keep_last: 0,
            summary_text: None,
            custom: None,
        };
        let result = project_transcript(&transcript, &policy).await.unwrap();
        assert!(result.provider_safety_blocked);
        // Falls back to identity: all messages survive.
        assert_eq!(result.kept_indices.len(), 5);
        assert_eq!(result.dropped_indices.len(), 0);
    }

    #[tokio::test]
    async fn provider_safety_can_be_disabled_for_local_only_previews() {
        let transcript = transcript_with(
            vec![
                serde_json::json!({"role": "user", "content": "first"}),
                serde_json::json!({
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "private chain", "signature": "abc"},
                        {"type": "tool_use", "id": "c", "name": "run", "input": {}}
                    ],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "c",
                    "name": "run",
                    "content": "Error",
                    "is_error": true,
                }),
                serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "c2", "name": "run", "arguments": {}}],
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "c2",
                    "name": "run",
                    "content": "ok",
                }),
            ],
            None,
        );
        let policy = ProjectionPolicy {
            kind: PolicyKind::CleanToolRepair,
            respect_provider_signatures: false,
            reason: None,
            summary_keep_last: 0,
            summary_text: None,
            custom: None,
        };
        let result = project_transcript(&transcript, &policy).await.unwrap();
        assert!(!result.provider_safety_blocked);
        assert!(!result.dropped_indices.is_empty());
    }

    #[tokio::test]
    async fn hash_changes_when_messages_change() {
        let raw_msgs = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "assistant", "content": "hey"}),
        ];
        let h1 = hash_messages(&raw_msgs);
        let h2 = hash_messages(&[
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "assistant", "content": "different"}),
        ]);
        assert_ne!(h1, h2);
        let h3 = hash_messages(&raw_msgs);
        assert_eq!(h1, h3);
    }

    #[test]
    fn canonical_json_orders_keys() {
        let a = serde_json::json!({"b": 1, "a": 2});
        let b = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn parse_policy_accepts_string_shorthand() {
        let policy =
            parse_projection_options(&VmValue::String(Rc::from("clean_tool_repair"))).unwrap();
        assert_eq!(policy.kind, PolicyKind::CleanToolRepair);
    }

    #[test]
    fn parse_policy_rejects_unknown_kind() {
        let err = parse_projection_options(&VmValue::String(Rc::from("bogus"))).unwrap_err();
        match err {
            VmError::Runtime(msg) => assert!(msg.contains("bogus")),
            _ => panic!("expected runtime error"),
        }
    }
}
