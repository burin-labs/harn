//! Artifact and run-record builtins.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::orchestration::{
    compute_handoff_effects, crystallize_traces, diff_run_records,
    evaluate_context_pack_suggestion_expectations, evaluate_eval_pack_manifest,
    evaluate_run_against_fixture, evaluate_run_suite, evaluate_run_suite_manifest,
    extract_handoff_from_artifact, generate_context_pack_suggestions, handoff_context_text,
    handoff_from_json_value, normalize_artifact, normalize_context_pack_manifest,
    normalize_eval_pack_manifest_value, normalize_eval_suite_manifest, normalize_friction_event,
    normalize_handoff_artifact_json, normalize_persona_eval_ladder_manifest_value,
    normalize_run_record, parse_context_pack_manifest_src, parse_friction_events_value,
    render_artifacts_context, render_unified_diff, replay_fixture_from_run,
    run_persona_eval_ladder, save_run_record, select_artifacts, synthesize_candidate_from_trace,
    ArtifactRecord, CapabilityPolicy, ContextPackSuggestionExpectation,
    ContextPackSuggestionOptions, ContextPolicy, CrystallizationTrace, CrystallizeOptions,
    ReplayFixture,
};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::workflow::load_run_tree;

/// A named metric recorded during evaluation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalMetric {
    pub name: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

thread_local! {
    static EVAL_METRICS: RefCell<Vec<EvalMetric>> = const { RefCell::new(Vec::new()) };
    static FRICTION_EVENTS: RefCell<Vec<crate::orchestration::FrictionEvent>> = const { RefCell::new(Vec::new()) };
}

/// Reset thread-local eval metrics. Call between test runs to avoid leaking.
pub fn reset_eval_metrics() {
    EVAL_METRICS.with(|m| m.borrow_mut().clear());
}

pub fn reset_friction_events() {
    FRICTION_EVENTS.with(|events| events.borrow_mut().clear());
}

/// Peek at recorded eval metrics without consuming them.
#[allow(dead_code)]
pub(crate) fn peek_eval_metrics() -> Vec<EvalMetric> {
    EVAL_METRICS.with(|m| m.borrow().clone())
}

fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("records encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

fn emit_handoff_event(artifact: &ArtifactRecord) {
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    let Some(handoff) = extract_handoff_from_artifact(artifact) else {
        return;
    };
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::Handoff {
        session_id,
        artifact_id: artifact.id.clone(),
        handoff: Box::new(handoff),
    });
}

fn attach_current_reminder_propagation(handoff: &mut crate::orchestration::HandoffArtifact) {
    if !handoff.reminder_propagation.is_empty() {
        return;
    }
    let Some(session_id) = crate::llm::current_agent_session_id() else {
        return;
    };
    let Some(transcript) = crate::agent_sessions::transcript(&session_id) else {
        return;
    };
    handoff.reminder_propagation =
        crate::llm::helpers::reminder_propagation_from_transcript(&transcript, &session_id);
}

pub(crate) fn parse_artifact_list(value: Option<&VmValue>) -> Result<Vec<ArtifactRecord>, VmError> {
    match value {
        Some(VmValue::List(list)) => list.iter().map(normalize_artifact).collect(),
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(_) => Err(VmError::Runtime(
            "expected artifact list or nil".to_string(),
        )),
    }
}

pub(crate) fn parse_context_policy(value: Option<&VmValue>) -> Result<ContextPolicy, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("context policy parse error: {e}"))),
        None => Ok(ContextPolicy::default()),
    }
}

#[derive(Default)]
struct ArtifactHelperOptions {
    id: Option<String>,
    title: Option<String>,
    text: Option<String>,
    source: Option<String>,
    stage: Option<String>,
    freshness: Option<String>,
    priority: Option<i64>,
    relevance: Option<f64>,
    estimated_tokens: Option<usize>,
    lineage: Vec<String>,
    metadata: BTreeMap<String, serde_json::Value>,
    data: Option<serde_json::Value>,
}

fn parse_artifact_helper_options(
    value: Option<&VmValue>,
) -> Result<ArtifactHelperOptions, VmError> {
    let Some(value) = value else {
        return Ok(ArtifactHelperOptions::default());
    };
    match value {
        VmValue::Nil => Ok(ArtifactHelperOptions::default()),
        VmValue::Dict(_) => {
            let json = crate::llm::vm_value_to_json(value);
            let mut options = ArtifactHelperOptions::default();
            let Some(map) = json.as_object() else {
                return Ok(options);
            };
            options.id = map
                .get("id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.title = map
                .get("title")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.text = map
                .get("text")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.source = map
                .get("source")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.stage = map
                .get("stage")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.freshness = map
                .get("freshness")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.priority = map.get("priority").and_then(|value| value.as_i64());
            options.relevance = map.get("relevance").and_then(|value| value.as_f64());
            options.estimated_tokens = map
                .get("estimated_tokens")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize);
            options.lineage = map
                .get("lineage")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(|item| item.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            options.metadata = map
                .get("metadata")
                .and_then(|value| value.as_object())
                .map(|meta| {
                    meta.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            options.data = map.get("data").cloned();
            Ok(options)
        }
        _ => Err(VmError::Runtime(
            "artifact helper options must be a dict or nil".to_string(),
        )),
    }
}

fn require_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    field: &str,
) -> Result<String, VmError> {
    args.get(index)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing {field}")))
}

fn require_text_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    field: &str,
) -> Result<String, VmError> {
    args.get(index)
        .map(value_to_text)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing {field}")))
}

fn optional_text_arg(value: Option<&VmValue>) -> Option<String> {
    value
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(value_to_text)
        .filter(|value| !value.is_empty())
}

fn value_to_text(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.to_string(),
        _ => {
            let json = crate::llm::vm_value_to_json(value);
            if let Some(text) = json.as_str() {
                text.to_string()
            } else {
                json.to_string()
            }
        }
    }
}

fn merge_json_value(
    base: Option<serde_json::Value>,
    overlay: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, overlay) {
        (Some(serde_json::Value::Object(mut base)), Some(serde_json::Value::Object(overlay))) => {
            for (key, value) in overlay {
                base.insert(key, value);
            }
            Some(serde_json::Value::Object(base))
        }
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
        (Some(_), Some(overlay)) => Some(overlay),
        (None, None) => None,
    }
}

fn build_helper_artifact(
    kind: &str,
    title: Option<String>,
    text: Option<String>,
    data: Option<serde_json::Value>,
    options: ArtifactHelperOptions,
) -> ArtifactRecord {
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: options.id.unwrap_or_default(),
        kind: kind.to_string(),
        title: options.title.or(title),
        text: options.text.or(text),
        data: merge_json_value(data, options.data),
        source: options.source,
        created_at: String::new(),
        freshness: options.freshness,
        priority: options.priority,
        lineage: options.lineage,
        relevance: options.relevance,
        estimated_tokens: options.estimated_tokens,
        stage: options.stage,
        metadata: options.metadata,
    }
    .normalize()
}

pub(crate) fn register_record_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "artifact(payload: dict) -> dict", category = "records")]
fn artifact_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let artifact = normalize_artifact(
        args.first()
            .ok_or_else(|| VmError::Runtime("artifact: missing artifact payload".to_string()))?,
    )?;
    emit_handoff_event(&artifact);
    to_vm(&artifact)
}

#[harn_builtin(sig = "handoff(payload: dict) -> dict", category = "records")]
fn handoff_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args
        .first()
        .ok_or_else(|| VmError::Runtime("handoff: missing payload".to_string()))?;
    let json = crate::llm::vm_value_to_json(value);
    let mut handoff = handoff_from_json_value(&json)
        .or_else(|| normalize_handoff_artifact_json(json.clone()).ok())
        .ok_or_else(|| VmError::Runtime("handoff: invalid handoff payload".to_string()))?;
    attach_current_reminder_propagation(&mut handoff);
    to_vm(&handoff)
}

#[harn_builtin(sig = "handoff_context(payload: dict) -> string", category = "records")]
fn handoff_context_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args
        .first()
        .ok_or_else(|| VmError::Runtime("handoff_context: missing payload".to_string()))?;
    let json = crate::llm::vm_value_to_json(value);
    let handoff = handoff_from_json_value(&json)
        .or_else(|| normalize_handoff_artifact_json(json.clone()).ok())
        .ok_or_else(|| VmError::Runtime("handoff_context: invalid handoff payload".to_string()))?;
    Ok(VmValue::String(std::sync::Arc::from(handoff_context_text(
        &handoff,
    ))))
}

#[harn_builtin(sig = "handoff_routes() -> list", category = "records")]
fn handoff_routes_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    to_vm(&crate::orchestration::snapshot_handoff_routes())
}

/// `handoff_effects(source, ceiling?)` — compute the typed effect set for a
/// child agent's entrypoint source. Pipelines call this when building a
/// spawn-time handoff so the envelope carries an explicit, ceiling-clamped
/// effect set (issue HARN-#1776 / E5.3). The dispatcher (E5.4) and the
/// OpenTrustGraph receipt chain (E5.5) consume this payload directly.
#[harn_builtin(
    sig = "handoff_effects(source: string, ceiling?: dict) -> list",
    category = "records"
)]
fn handoff_effects_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = match args.first() {
        Some(VmValue::String(text)) => text.to_string(),
        Some(VmValue::Nil) | None => String::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "handoff_effects: expected source string, got {}",
                other.type_name()
            )));
        }
    };
    let ceiling = match args.get(1) {
        Some(VmValue::Nil) | None => None,
        Some(value) => {
            let json = crate::llm::vm_value_to_json(value);
            Some(
                serde_json::from_value::<CapabilityPolicy>(json).map_err(|error| {
                    VmError::Runtime(format!("handoff_effects: ceiling parse error: {error}"))
                })?,
            )
        }
    };
    let effects = compute_handoff_effects(&source, ceiling.as_ref());
    to_vm(&effects)
}

#[harn_builtin(
    sig = "artifact_derive(parent: dict, kind?: string, extras?: dict) -> dict",
    category = "records"
)]
fn artifact_derive_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let parent = normalize_artifact(
        args.first()
            .ok_or_else(|| VmError::Runtime("artifact_derive: missing parent".to_string()))?,
    )?;
    let kind = args
        .get(1)
        .map(|v| v.display())
        .unwrap_or_else(|| "artifact".to_string());
    let mut derived = parent.clone();
    derived.id = format!("{}_derived", parent.id);
    derived.kind = kind;
    derived.lineage.push(parent.id);
    if let Some(VmValue::Dict(extra)) = args.get(2) {
        let extra_json = crate::llm::vm_value_to_json(&VmValue::Dict(extra.clone()));
        if let Some(text) = extra_json.get("text").and_then(|v| v.as_str()) {
            derived.text = Some(text.to_string());
        }
    }
    to_vm(&derived.normalize())
}

#[harn_builtin(
    sig = "artifact_select(artifacts: list, policy?: dict) -> list",
    category = "records"
)]
fn artifact_select_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let artifacts = parse_artifact_list(args.first())?;
    let policy = parse_context_policy(args.get(1))?;
    to_vm(&select_artifacts(artifacts, &policy))
}

#[harn_builtin(
    sig = "artifact_context(artifacts: list, policy?: dict) -> string",
    category = "records"
)]
fn artifact_context_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let artifacts = parse_artifact_list(args.first())?;
    let policy = parse_context_policy(args.get(1))?;
    Ok(VmValue::String(std::sync::Arc::from(
        render_artifacts_context(&select_artifacts(artifacts, &policy), &policy),
    )))
}

#[harn_builtin(
    sig = "artifact_workspace_file(path: string?, content: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_workspace_file_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = require_string_arg(args, 0, "artifact_workspace_file", "path")?;
    let content = require_text_arg(args, 1, "artifact_workspace_file", "content")?;
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options
        .metadata
        .insert("path".to_string(), serde_json::json!(path));
    let artifact = build_helper_artifact(
        "workspace_file",
        Some(path.clone()),
        Some(content.clone()),
        Some(serde_json::json!({"path": path, "content": content})),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_workspace_snapshot(paths: any, summary?: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_workspace_snapshot_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let paths = args.first().ok_or_else(|| {
        VmError::Runtime("artifact_workspace_snapshot: missing paths".to_string())
    })?;
    let summary = optional_text_arg(args.get(1));
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options
        .metadata
        .insert("paths".to_string(), crate::llm::vm_value_to_json(paths));
    let artifact = build_helper_artifact(
        "workspace_snapshot",
        Some("workspace snapshot".to_string()),
        summary,
        Some(serde_json::json!({"paths": crate::llm::vm_value_to_json(paths)})),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_editor_selection(path: string?, text: string?, options?: dict) -> dict",
    category = "records"
)]
fn artifact_editor_selection_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = require_string_arg(args, 0, "artifact_editor_selection", "path")?;
    let text = require_text_arg(args, 1, "artifact_editor_selection", "text")?;
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options
        .metadata
        .insert("path".to_string(), serde_json::json!(path));
    let artifact = build_helper_artifact(
        "editor_selection",
        Some(format!("selection {path}")),
        Some(text.clone()),
        Some(serde_json::json!({"path": path, "text": text})),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_verification_result(title: string, text: string?, options?: dict) -> dict",
    category = "records"
)]
fn artifact_verification_result_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let title = require_string_arg(args, 0, "artifact_verification_result", "title")?;
    let text = require_text_arg(args, 1, "artifact_verification_result", "text")?;
    let artifact = build_helper_artifact(
        "verification_result",
        Some(title.clone()),
        Some(text.clone()),
        Some(serde_json::json!({"title": title, "text": text})),
        parse_artifact_helper_options(args.get(2))?,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_test_result(title: string, text: string?, options?: dict) -> dict",
    category = "records"
)]
fn artifact_test_result_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let title = require_string_arg(args, 0, "artifact_test_result", "title")?;
    let text = require_text_arg(args, 1, "artifact_test_result", "text")?;
    let artifact = build_helper_artifact(
        "test_result",
        Some(title.clone()),
        Some(text.clone()),
        Some(serde_json::json!({"title": title, "text": text})),
        parse_artifact_helper_options(args.get(2))?,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_command_result(command: string, output: any, options?: dict) -> dict",
    category = "records"
)]
fn artifact_command_result_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let command = require_string_arg(args, 0, "artifact_command_result", "command")?;
    let output = args
        .get(1)
        .ok_or_else(|| VmError::Runtime("artifact_command_result: missing output".to_string()))?;
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options
        .metadata
        .insert("command".to_string(), serde_json::json!(command));
    let artifact = build_helper_artifact(
        "command_result",
        Some(command.clone()),
        Some(value_to_text(output)),
        Some(serde_json::json!({
            "command": command,
            "output": crate::llm::vm_value_to_json(output)
        })),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_diff(path: string?, before: string, after: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_diff_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = require_string_arg(args, 0, "artifact_diff", "path")?;
    let before = require_text_arg(args, 1, "artifact_diff", "before")?;
    let after = require_text_arg(args, 2, "artifact_diff", "after")?;
    let mut options = parse_artifact_helper_options(args.get(3))?;
    options
        .metadata
        .insert("path".to_string(), serde_json::json!(path));
    let artifact = build_helper_artifact(
        "diff",
        Some(format!("diff {path}")),
        Some(render_unified_diff(Some(&path), &before, &after)),
        Some(serde_json::json!({"path": path, "before": before, "after": after})),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_git_diff(diff_text: string?, options?: dict) -> dict",
    category = "records"
)]
fn artifact_git_diff_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let diff_text = require_text_arg(args, 0, "artifact_git_diff", "diff_text")?;
    let artifact = build_helper_artifact(
        "git_diff",
        Some("git diff".to_string()),
        Some(diff_text.clone()),
        Some(serde_json::json!({"diff": diff_text})),
        parse_artifact_helper_options(args.get(1))?,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_diff_review(target: dict, summary?: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_diff_review_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = normalize_artifact(args.first().ok_or_else(|| {
        VmError::Runtime("artifact_diff_review: missing target artifact".to_string())
    })?)?;
    let summary = optional_text_arg(args.get(1));
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options.lineage.extend(target.lineage.clone());
    options.lineage.push(target.id.clone());
    options.metadata.insert(
        "target_artifact_id".to_string(),
        serde_json::json!(target.id),
    );
    options
        .metadata
        .insert("target_kind".to_string(), serde_json::json!(target.kind));
    let artifact = build_helper_artifact(
        "diff_review",
        Some(format!(
            "review {}",
            target.title.clone().unwrap_or_else(|| target.id.clone())
        )),
        summary,
        Some(serde_json::json!({"target_artifact_id": target.id, "target_kind": target.kind})),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_review_decision(target: dict, decision: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_review_decision_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = normalize_artifact(args.first().ok_or_else(|| {
        VmError::Runtime("artifact_review_decision: missing target artifact".to_string())
    })?)?;
    let decision = require_string_arg(args, 1, "artifact_review_decision", "decision")?;
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options.lineage.extend(target.lineage.clone());
    options.lineage.push(target.id.clone());
    options.metadata.insert(
        "target_artifact_id".to_string(),
        serde_json::json!(target.id),
    );
    options
        .metadata
        .insert("target_kind".to_string(), serde_json::json!(target.kind));
    options
        .metadata
        .insert("decision".to_string(), serde_json::json!(decision));
    let artifact = build_helper_artifact(
        "review_decision",
        Some(format!(
            "{} {}",
            decision,
            target.title.clone().unwrap_or_else(|| target.id.clone())
        )),
        Some(decision.clone()),
        Some(serde_json::json!({
            "target_artifact_id": target.id,
            "target_kind": target.kind,
            "decision": decision
        })),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_patch_proposal(target: dict, patch: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_patch_proposal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = normalize_artifact(args.first().ok_or_else(|| {
        VmError::Runtime("artifact_patch_proposal: missing target artifact".to_string())
    })?)?;
    let patch = require_text_arg(args, 1, "artifact_patch_proposal", "patch")?;
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options.lineage.extend(target.lineage.clone());
    options.lineage.push(target.id.clone());
    options.metadata.insert(
        "target_artifact_id".to_string(),
        serde_json::json!(target.id),
    );
    options
        .metadata
        .insert("target_kind".to_string(), serde_json::json!(target.kind));
    let artifact = build_helper_artifact(
        "patch_proposal",
        Some(format!(
            "patch for {}",
            target.title.clone().unwrap_or_else(|| target.id.clone())
        )),
        Some(patch.clone()),
        Some(serde_json::json!({
            "target_artifact_id": target.id,
            "target_kind": target.kind,
            "patch": patch
        })),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_verification_bundle(title: string, checks: any, options?: dict) -> dict",
    category = "records"
)]
fn artifact_verification_bundle_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let title = require_string_arg(args, 0, "artifact_verification_bundle", "title")?;
    let checks = args.get(1).ok_or_else(|| {
        VmError::Runtime("artifact_verification_bundle: missing checks".to_string())
    })?;
    let artifact = build_helper_artifact(
        "verification_bundle",
        Some(title.clone()),
        Some(value_to_text(checks)),
        Some(serde_json::json!({
            "title": title,
            "checks": crate::llm::vm_value_to_json(checks)
        })),
        parse_artifact_helper_options(args.get(2))?,
    );
    to_vm(&artifact)
}

#[harn_builtin(
    sig = "artifact_apply_intent(target: dict, intent: string, options?: dict) -> dict",
    category = "records"
)]
fn artifact_apply_intent_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = normalize_artifact(args.first().ok_or_else(|| {
        VmError::Runtime("artifact_apply_intent: missing target artifact".to_string())
    })?)?;
    let intent = require_string_arg(args, 1, "artifact_apply_intent", "intent")?;
    let mut options = parse_artifact_helper_options(args.get(2))?;
    options.lineage.extend(target.lineage.clone());
    options.lineage.push(target.id.clone());
    options.metadata.insert(
        "target_artifact_id".to_string(),
        serde_json::json!(target.id),
    );
    options
        .metadata
        .insert("target_kind".to_string(), serde_json::json!(target.kind));
    let artifact = build_helper_artifact(
        "apply_intent",
        Some(format!(
            "{} {}",
            intent,
            target.title.clone().unwrap_or_else(|| target.id.clone())
        )),
        Some(intent.clone()),
        Some(serde_json::json!({
            "target_artifact_id": target.id,
            "target_kind": target.kind,
            "intent": intent
        })),
        options,
    );
    to_vm(&artifact)
}

#[harn_builtin(sig = "run_record(payload: dict) -> dict", category = "records")]
fn run_record_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let run = normalize_run_record(
        args.first()
            .ok_or_else(|| VmError::Runtime("run_record: missing payload".to_string()))?,
    )?;
    to_vm(&run)
}

#[harn_builtin(sig = "load_run_tree(path: string?) -> dict", category = "records")]
fn load_run_tree_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = require_string_arg(args, 0, "load_run_tree", "path")?;
    let tree = load_run_tree(&path)?;
    to_vm(&tree)
}

#[harn_builtin(
    sig = "run_record_save(run: dict, path?: string) -> dict",
    category = "records"
)]
fn run_record_save_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut run = normalize_run_record(
        args.first()
            .ok_or_else(|| VmError::Runtime("run_record_save: missing run".to_string()))?,
    )?;
    let path = args.get(1).map(|v| v.display()).filter(|s| !s.is_empty());
    let persisted = save_run_record(&run, path.as_deref())?;
    run.persisted_path = Some(persisted.clone());
    to_vm(&serde_json::json!({"path": persisted, "run": run}))
}

#[harn_builtin(sig = "run_record_load(path: string?) -> dict", category = "records")]
fn run_record_load_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("run_record_load: missing path".to_string()))?;
    to_vm(&crate::orchestration::load_run_record(
        std::path::Path::new(&path),
    )?)
}

#[harn_builtin(sig = "run_record_fixture(run: dict) -> dict", category = "records")]
fn run_record_fixture_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let run = normalize_run_record(
        args.first()
            .ok_or_else(|| VmError::Runtime("run_record_fixture: missing run".to_string()))?,
    )?;
    to_vm(&replay_fixture_from_run(&run))
}

#[harn_builtin(
    sig = "run_record_eval(run: dict, fixture?: dict) -> dict",
    category = "records"
)]
fn run_record_eval_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let run = normalize_run_record(
        args.first()
            .ok_or_else(|| VmError::Runtime("run_record_eval: missing run".to_string()))?,
    )?;
    let fixture: ReplayFixture = match args.get(1) {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("run_record_eval: {e}")))?,
        None => replay_fixture_from_run(&run),
    };
    to_vm(&evaluate_run_against_fixture(&run, &fixture))
}

#[harn_builtin(
    sig = "run_record_eval_suite(cases: list) -> dict",
    category = "records"
)]
fn run_record_eval_suite_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let items = match args.first() {
        Some(VmValue::List(list)) => list.clone(),
        _ => {
            return Err(VmError::Runtime(
                "run_record_eval_suite: missing list".to_string(),
            ));
        }
    };
    let mut cases = Vec::new();
    for item in items.iter() {
        let source_path = item
            .as_dict()
            .and_then(|dict| dict.get("path"))
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
        let run = if let Some(dict) = item.as_dict() {
            if let Some(run_value) = dict.get("run") {
                normalize_run_record(run_value)?
            } else {
                normalize_run_record(item)?
            }
        } else {
            normalize_run_record(item)?
        };
        let fixture: ReplayFixture = match item.as_dict().and_then(|dict| dict.get("fixture")) {
            Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
                .map_err(|e| VmError::Runtime(format!("run_record_eval_suite: {e}")))?,
            None => replay_fixture_from_run(&run),
        };
        cases.push((run, fixture, source_path));
    }
    to_vm(&evaluate_run_suite(cases))
}

#[harn_builtin(
    sig = "run_record_diff(left: dict, right: dict) -> dict",
    category = "records"
)]
fn run_record_diff_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let left = normalize_run_record(
        args.first()
            .ok_or_else(|| VmError::Runtime("run_record_diff: missing left run".to_string()))?,
    )?;
    let right = normalize_run_record(
        args.get(1)
            .ok_or_else(|| VmError::Runtime("run_record_diff: missing right run".to_string()))?,
    )?;
    to_vm(&diff_run_records(&left, &right))
}

#[harn_builtin(
    sig = "eval_suite_manifest(payload: dict) -> dict",
    category = "records"
)]
fn eval_suite_manifest_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest = normalize_eval_suite_manifest(args.first().ok_or_else(|| {
        VmError::Runtime("eval_suite_manifest: missing manifest payload".to_string())
    })?)?;
    to_vm(&manifest)
}

#[harn_builtin(sig = "eval_suite_run(payload: dict) -> dict", category = "records")]
fn eval_suite_run_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest = normalize_eval_suite_manifest(args.first().ok_or_else(|| {
        VmError::Runtime("eval_suite_run: missing manifest payload".to_string())
    })?)?;
    to_vm(&evaluate_run_suite_manifest(&manifest)?)
}

#[harn_builtin(
    sig = "eval_pack_manifest(payload: dict) -> dict",
    category = "records"
)]
fn eval_pack_manifest_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest = normalize_eval_pack_manifest_value(args.first().ok_or_else(|| {
        VmError::Runtime("eval_pack_manifest: missing manifest payload".to_string())
    })?)?;
    to_vm(&manifest)
}

#[harn_builtin(sig = "eval_pack_run(payload: dict) -> dict", category = "records")]
fn eval_pack_run_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest =
        normalize_eval_pack_manifest_value(args.first().ok_or_else(|| {
            VmError::Runtime("eval_pack_run: missing manifest payload".to_string())
        })?)?;
    to_vm(&evaluate_eval_pack_manifest(&manifest)?)
}

#[harn_builtin(sig = "skill_induce(payload: dict) -> dict", category = "records")]
fn skill_induce_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let payload_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("skill_induce: missing payload dict".to_string()))?;
    let payload_json = crate::llm::vm_value_to_json(payload_value);
    let payload = payload_json
        .as_object()
        .ok_or_else(|| VmError::Runtime("skill_induce: payload must be a dict".to_string()))?;
    let traces = parse_skill_induce_traces(payload)?;
    let options = parse_skill_induce_options(payload)?;
    let artifacts = if traces.len() == 1 {
        synthesize_candidate_from_trace(
            traces.into_iter().next().expect("len checked"),
            options,
            Vec::new(),
            None,
            None,
        )?
    } else {
        crystallize_traces(traces, options)?
    };
    let report = artifacts.report;
    to_vm(&serde_json::json!({
        "accepted": !report.skill_candidates.is_empty(),
        "skill_candidates": report.skill_candidates,
        "rejected_skill_candidates": report.rejected_skill_candidates,
        "selected_candidate_id": report.selected_candidate_id,
    }))
}

fn parse_skill_induce_traces(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<CrystallizationTrace>, VmError> {
    let value = payload
        .get("traces")
        .or_else(|| payload.get("trajectory"))
        .or_else(|| payload.get("trace"))
        .ok_or_else(|| {
            VmError::Runtime(
                "skill_induce: payload requires `traces`, `trajectory`, or `trace`".to_string(),
            )
        })?;
    let traces = match value {
        serde_json::Value::Array(_) => {
            serde_json::from_value::<Vec<CrystallizationTrace>>(value.clone())
        }
        _ => serde_json::from_value::<CrystallizationTrace>(value.clone()).map(|trace| vec![trace]),
    }
    .map_err(|error| VmError::Runtime(format!("skill_induce: invalid trace payload: {error}")))?;
    if traces.is_empty() {
        return Err(VmError::Runtime(
            "skill_induce: at least one trace is required".to_string(),
        ));
    }
    Ok(traces)
}

fn parse_skill_induce_options(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<CrystallizeOptions, VmError> {
    let mut options = CrystallizeOptions::default();
    let empty = serde_json::Map::new();
    let opts = payload
        .get("options")
        .and_then(serde_json::Value::as_object)
        .unwrap_or(&empty);
    options.min_examples = json_usize(
        payload
            .get("min_examples")
            .or_else(|| opts.get("min_examples")),
    )
    .unwrap_or_default();
    options.promotion_min_confidence = json_f64(
        payload
            .get("promotion_min_confidence")
            .or_else(|| opts.get("promotion_min_confidence")),
    )
    .unwrap_or_default();
    options.workflow_name = json_string(
        payload
            .get("workflow_name")
            .or_else(|| opts.get("workflow_name")),
    );
    options.package_name = json_string(
        payload
            .get("package_name")
            .or_else(|| opts.get("package_name")),
    );
    options.author = json_string(payload.get("author").or_else(|| opts.get("author")));
    options.approver = json_string(payload.get("approver").or_else(|| opts.get("approver")));
    options.eval_pack_link = json_string(
        payload
            .get("eval_pack_link")
            .or_else(|| opts.get("eval_pack_link")),
    );
    let heldout = payload
        .get("heldout_traces")
        .or_else(|| payload.get("shadow_traces"))
        .or_else(|| opts.get("heldout_traces"))
        .or_else(|| opts.get("shadow_traces"));
    if let Some(value) = heldout {
        options.shadow_traces = serde_json::from_value::<Vec<CrystallizationTrace>>(value.clone())
            .map_err(|error| {
                VmError::Runtime(format!("skill_induce: invalid heldout_traces: {error}"))
            })?;
    }
    Ok(options)
}

fn json_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_usize(value: Option<&serde_json::Value>) -> Option<usize> {
    value
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
}

fn json_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(serde_json::Value::as_f64)
}

#[harn_builtin(
    sig = "persona_eval_ladder_manifest(payload: dict) -> dict",
    category = "records"
)]
fn persona_eval_ladder_manifest_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let manifest =
        normalize_persona_eval_ladder_manifest_value(args.first().ok_or_else(|| {
            VmError::Runtime("persona_eval_ladder_manifest: missing manifest payload".to_string())
        })?)?;
    to_vm(&manifest)
}

#[harn_builtin(
    sig = "persona_eval_ladder_run(payload: dict) -> dict",
    category = "records"
)]
fn persona_eval_ladder_run_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest =
        normalize_persona_eval_ladder_manifest_value(args.first().ok_or_else(|| {
            VmError::Runtime("persona_eval_ladder_run: missing manifest payload".to_string())
        })?)?;
    to_vm(&run_persona_eval_ladder(&manifest)?)
}

#[harn_builtin(sig = "friction_event(payload: dict) -> dict", category = "records")]
fn friction_event_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let event = normalize_friction_event(
        args.first()
            .ok_or_else(|| VmError::Runtime("friction_event: missing payload".to_string()))?,
    )?;
    to_vm(&event)
}

#[harn_builtin(
    sig = "friction_record(payload: dict, options?: dict) -> dict",
    category = "records"
)]
fn friction_record_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let event = normalize_friction_event(
        args.first()
            .ok_or_else(|| VmError::Runtime("friction_record: missing payload".to_string()))?,
    )?;
    let options = args.get(1).map(crate::llm::vm_value_to_json);
    let enabled = options
        .as_ref()
        .and_then(|value| value.get("enabled"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if !enabled {
        return to_vm(&serde_json::json!({
            "recorded": false,
            "sink": "disabled",
            "event": event
        }));
    }

    FRICTION_EVENTS.with(|events| events.borrow_mut().push(event.clone()));

    let path = options
        .as_ref()
        .and_then(|value| value.get("log_path").or_else(|| value.get("path")))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| std::env::var("HARN_FRICTION_LOG").ok());
    if let Some(path) = path {
        let encoded = serde_json::to_string(&event)
            .map_err(|e| VmError::Runtime(format!("friction_record: encode error: {e}")))?;
        if let Some(parent) = std::path::Path::new(&path).parent() {
            if parent.as_os_str().is_empty() {
                // Relative filename in cwd: no directory to create.
            } else {
                std::fs::create_dir_all(parent).map_err(|e| {
                    VmError::Runtime(format!("friction_record: failed to create log dir: {e}"))
                })?;
            }
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| VmError::Runtime(format!("friction_record: failed to open log: {e}")))?;
        writeln!(file, "{encoded}")
            .map_err(|e| VmError::Runtime(format!("friction_record: failed to append log: {e}")))?;
        return to_vm(&serde_json::json!({
            "recorded": true,
            "sink": "jsonl",
            "path": path,
            "event": event
        }));
    }

    to_vm(&serde_json::json!({
        "recorded": true,
        "sink": "memory",
        "event": event
    }))
}

#[harn_builtin(sig = "friction_events() -> list", category = "records")]
fn friction_events_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let events = FRICTION_EVENTS.with(|events| events.borrow().clone());
    to_vm(&events)
}

#[harn_builtin(sig = "friction_clear() -> nil", category = "records")]
fn friction_clear_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    FRICTION_EVENTS.with(|events| events.borrow_mut().clear());
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "context_pack_manifest(payload: dict) -> dict",
    category = "records"
)]
fn context_pack_manifest_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest =
        normalize_context_pack_manifest(args.first().ok_or_else(|| {
            VmError::Runtime("context_pack_manifest: missing payload".to_string())
        })?)?;
    to_vm(&manifest)
}

#[harn_builtin(
    sig = "context_pack_manifest_parse(source: string) -> dict",
    category = "records"
)]
fn context_pack_manifest_parse_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let src = require_text_arg(args, 0, "context_pack_manifest_parse", "source")?;
    let manifest = parse_context_pack_manifest_src(&src)?;
    to_vm(&manifest)
}

#[harn_builtin(
    sig = "context_pack_suggestions(events?: list, options?: dict) -> list",
    category = "records"
)]
fn context_pack_suggestions_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let events = match args.first() {
        Some(value) => parse_friction_events_value(value)?,
        None => FRICTION_EVENTS.with(|events| Ok::<_, VmError>(events.borrow().clone()))?,
    };
    let options: ContextPackSuggestionOptions = match args.get(1) {
        Some(value) if !matches!(value, VmValue::Nil) => {
            serde_json::from_value(crate::llm::vm_value_to_json(value)).map_err(|e| {
                VmError::Runtime(format!(
                    "context_pack_suggestions: options parse error: {e}"
                ))
            })?
        }
        _ => ContextPackSuggestionOptions::default(),
    };
    to_vm(&generate_context_pack_suggestions(&events, &options))
}

#[harn_builtin(
    sig = "friction_eval_fixture(fixture: dict) -> dict",
    category = "records"
)]
fn friction_eval_fixture_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let fixture = args.first().ok_or_else(|| {
        VmError::Runtime("friction_eval_fixture: missing fixture payload".to_string())
    })?;
    let json = crate::llm::vm_value_to_json(fixture);
    let events = parse_friction_events_value(fixture)?;
    let options: ContextPackSuggestionOptions = json
        .get("options")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| VmError::Runtime(format!("friction_eval_fixture: options parse error: {e}")))?
        .unwrap_or_default();
    let expectations: Vec<ContextPackSuggestionExpectation> = json
        .get("expected_suggestions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| {
            VmError::Runtime(format!(
                "friction_eval_fixture: expected_suggestions parse error: {e}"
            ))
        })?
        .unwrap_or_default();
    let suggestions = generate_context_pack_suggestions(&events, &options);
    let failures = evaluate_context_pack_suggestion_expectations(&suggestions, &expectations);
    to_vm(&serde_json::json!({
        "pass": failures.is_empty(),
        "failures": failures,
        "event_count": events.len(),
        "suggestion_count": suggestions.len(),
        "suggestions": suggestions
    }))
}

#[harn_builtin(
    sig = "eval_metric(name: string, value: any, metadata?: dict) -> nil",
    category = "records"
)]
fn eval_metric_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args
        .first()
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime("eval_metric: missing name".to_string()))?;
    let value = args
        .get(1)
        .ok_or_else(|| VmError::Runtime("eval_metric: missing value".to_string()))?;
    let value_json = crate::llm::vm_value_to_json(value);
    let metadata = args
        .get(2)
        .filter(|v| !matches!(v, VmValue::Nil))
        .map(crate::llm::vm_value_to_json);
    EVAL_METRICS.with(|m| {
        m.borrow_mut().push(EvalMetric {
            name,
            value: value_json,
            metadata,
        });
    });
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "eval_metrics() -> list", category = "records")]
fn eval_metrics_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let metrics = EVAL_METRICS.with(|m| m.borrow().clone());
    let list: Vec<VmValue> = metrics
        .iter()
        .map(|metric| {
            let mut dict = BTreeMap::new();
            dict.insert(
                "name".to_string(),
                VmValue::String(std::sync::Arc::from(metric.name.as_str())),
            );
            dict.insert(
                "value".to_string(),
                crate::stdlib::json_to_vm_value(&metric.value),
            );
            if let Some(ref meta) = metric.metadata {
                dict.insert(
                    "metadata".to_string(),
                    crate::stdlib::json_to_vm_value(meta),
                );
            }
            VmValue::Dict(std::sync::Arc::new(dict))
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::from(list)))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &ARTIFACT_IMPL_DEF,
    &HANDOFF_IMPL_DEF,
    &HANDOFF_CONTEXT_IMPL_DEF,
    &HANDOFF_ROUTES_IMPL_DEF,
    &HANDOFF_EFFECTS_IMPL_DEF,
    &ARTIFACT_DERIVE_IMPL_DEF,
    &ARTIFACT_SELECT_IMPL_DEF,
    &ARTIFACT_CONTEXT_IMPL_DEF,
    &ARTIFACT_WORKSPACE_FILE_IMPL_DEF,
    &ARTIFACT_WORKSPACE_SNAPSHOT_IMPL_DEF,
    &ARTIFACT_EDITOR_SELECTION_IMPL_DEF,
    &ARTIFACT_VERIFICATION_RESULT_IMPL_DEF,
    &ARTIFACT_TEST_RESULT_IMPL_DEF,
    &ARTIFACT_COMMAND_RESULT_IMPL_DEF,
    &ARTIFACT_DIFF_IMPL_DEF,
    &ARTIFACT_GIT_DIFF_IMPL_DEF,
    &ARTIFACT_DIFF_REVIEW_IMPL_DEF,
    &ARTIFACT_REVIEW_DECISION_IMPL_DEF,
    &ARTIFACT_PATCH_PROPOSAL_IMPL_DEF,
    &ARTIFACT_VERIFICATION_BUNDLE_IMPL_DEF,
    &ARTIFACT_APPLY_INTENT_IMPL_DEF,
    &RUN_RECORD_IMPL_DEF,
    &LOAD_RUN_TREE_IMPL_DEF,
    &RUN_RECORD_SAVE_IMPL_DEF,
    &RUN_RECORD_LOAD_IMPL_DEF,
    &RUN_RECORD_FIXTURE_IMPL_DEF,
    &RUN_RECORD_EVAL_IMPL_DEF,
    &RUN_RECORD_EVAL_SUITE_IMPL_DEF,
    &RUN_RECORD_DIFF_IMPL_DEF,
    &EVAL_SUITE_MANIFEST_IMPL_DEF,
    &EVAL_SUITE_RUN_IMPL_DEF,
    &EVAL_PACK_MANIFEST_IMPL_DEF,
    &EVAL_PACK_RUN_IMPL_DEF,
    &SKILL_INDUCE_IMPL_DEF,
    &PERSONA_EVAL_LADDER_MANIFEST_IMPL_DEF,
    &PERSONA_EVAL_LADDER_RUN_IMPL_DEF,
    &FRICTION_EVENT_IMPL_DEF,
    &FRICTION_RECORD_IMPL_DEF,
    &FRICTION_EVENTS_IMPL_DEF,
    &FRICTION_CLEAR_IMPL_DEF,
    &CONTEXT_PACK_MANIFEST_IMPL_DEF,
    &CONTEXT_PACK_MANIFEST_PARSE_IMPL_DEF,
    &CONTEXT_PACK_SUGGESTIONS_IMPL_DEF,
    &FRICTION_EVAL_FIXTURE_IMPL_DEF,
    &EVAL_METRIC_IMPL_DEF,
    &EVAL_METRICS_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_induce_trace(id: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "source_hash": format!("sha256:{id}"),
            "actions": [
                {
                    "id": format!("{id}-branch"),
                    "kind": "tool_call",
                    "name": "git.checkout_branch",
                    "parameters": {
                        "branch_name": format!("release-{version}")
                    },
                    "side_effects": [
                        {
                            "kind": "git_ref",
                            "target": "release-branch",
                            "capability": "git.write"
                        }
                    ],
                    "capabilities": ["git.write"],
                    "deterministic": true
                },
                {
                    "id": format!("{id}-manifest"),
                    "kind": "file_mutation",
                    "name": "update_manifest_version",
                    "parameters": {
                        "version": version
                    },
                    "side_effects": [
                        {
                            "kind": "file_write",
                            "target": "harn.toml",
                            "capability": "fs.write"
                        }
                    ],
                    "capabilities": ["fs.write"],
                    "deterministic": true
                }
            ]
        })
    }

    #[test]
    fn skill_induce_accepts_single_trace_with_heldout_sibling() {
        let payload = serde_json::json!({
            "trace": skill_induce_trace("source_trace", "0.9.1"),
            "heldout_traces": [skill_induce_trace("heldout_trace", "0.9.2")],
            "options": {
                "workflow_name": "release_package_maintenance",
                "package_name": "release-workflows"
            }
        });

        let result = skill_induce_impl(
            &[crate::stdlib::json_to_vm_value(&payload)],
            &mut String::new(),
        )
        .expect("skill_induce");
        let result = result.as_dict().expect("skill_induce result dict");
        assert!(matches!(result.get("accepted"), Some(VmValue::Bool(true))));

        let Some(VmValue::List(skills)) = result.get("skill_candidates") else {
            panic!("skill_candidates should be a list");
        };
        assert_eq!(skills.len(), 1);

        let skill = skills[0].as_dict().expect("skill candidate dict");
        let replay_gate = skill
            .get("replay_gate")
            .and_then(VmValue::as_dict)
            .expect("replay_gate dict");
        assert_eq!(
            replay_gate
                .get("original_trace_count")
                .and_then(VmValue::as_int),
            Some(1)
        );
        assert_eq!(
            replay_gate
                .get("heldout_trace_count")
                .and_then(VmValue::as_int),
            Some(1)
        );

        let receipt = replay_gate
            .get("receipt")
            .and_then(VmValue::as_dict)
            .expect("receipt dict");
        assert!(matches!(receipt.get("accepted"), Some(VmValue::Bool(true))));
    }
}
