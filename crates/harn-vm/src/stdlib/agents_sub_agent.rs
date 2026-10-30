use super::agents_workers;
use super::sub_agent_lifecycle as lifecycle;
use super::{SubAgentExecutionResult, SubAgentRunSpec};
use crate::orchestration::CapabilityPolicy;
#[cfg(test)]
use crate::orchestration::{annotate_nested_execution_options, NestedExecutionKind};
use crate::stdlib::args::{ErrorKind, Options};
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;
use lifecycle::{
    finish_sub_agent, normalize_run_identity, stop_details_for_error, stop_details_for_result,
    SubagentStopDetails,
};

#[path = "agents_sub_agent_string_list.rs"]
mod string_list;
use string_list::parse_string_list;

#[path = "agents_sub_agent_child_error.rs"]
mod child_error;

const SUB_AGENT_RUN_FN: &str = "sub_agent_run";
#[cfg(test)]
#[path = "agents_sub_agent_lifecycle_tests.rs"]
mod lifecycle_tests;
pub(super) struct ParsedSubAgentRequest {
    pub(super) spec: SubAgentRunSpec,
    pub(super) background: bool,
    pub(super) carry_policy: agents_workers::WorkerCarryPolicy,
    pub(super) execution: agents_workers::WorkerExecutionProfile,
    pub(super) worker_policy: Option<CapabilityPolicy>,
}

struct SubAgentPolicyResolution {
    requested_policy: Option<CapabilityPolicy>,
    worker_policy: Option<CapabilityPolicy>,
    carry_policy: agents_workers::WorkerCarryPolicy,
    execution: agents_workers::WorkerExecutionProfile,
}

fn sub_agent_requested_policy(
    policy_value: Option<&VmValue>,
    allowed_tools: &[String],
) -> Result<Option<CapabilityPolicy>, VmError> {
    let explicit: Option<CapabilityPolicy> = policy_value
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(|value| serde_json::from_value(crate::llm::vm_value_to_json(value)))
        .transpose()
        .map_err(|e| VmError::Runtime(format!("{SUB_AGENT_RUN_FN}: policy parse error: {e}")))?;
    let tool_policy = if allowed_tools.is_empty() {
        None
    } else {
        Some(CapabilityPolicy {
            tools: allowed_tools.to_vec(),
            ..Default::default()
        })
    };
    match (explicit, tool_policy) {
        (Some(policy), Some(tool_policy)) => policy
            .intersect(&tool_policy)
            .map(Some)
            .map_err(VmError::Runtime),
        (Some(policy), None) => Ok(Some(policy)),
        (None, Some(tool_policy)) => Ok(Some(tool_policy)),
        (None, None) => Ok(None),
    }
}

fn non_empty_raw_string(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

fn parse_reminder_propagation_value(
    value: &VmValue,
) -> Result<crate::llm::helpers::SystemReminder, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value)).map_err(|error| {
        VmError::Runtime(format!(
            "{SUB_AGENT_RUN_FN}: reminder_propagation parse error: {error}"
        ))
    })
}

fn inherited_reminders_from_parent(
    parent_session_id: Option<&str>,
) -> Vec<crate::llm::helpers::SystemReminder> {
    let Some(parent_session_id) = parent_session_id else {
        return Vec::new();
    };
    let Some(transcript) = crate::agent_sessions::transcript(parent_session_id) else {
        return Vec::new();
    };
    crate::llm::helpers::reminder_propagation_from_transcript(&transcript, parent_session_id)
}

pub(super) fn parse_sub_agent_request(args: &[VmValue]) -> Result<ParsedSubAgentRequest, VmError> {
    let request = validate_sub_agent_request_envelope(args)?;
    let mut parser = Options::new(SUB_AGENT_RUN_FN, ErrorKind::Runtime, Some(request));
    if parser.opt_string("_type")? != Some("sub_agent_request") {
        return Err(invalid_sub_agent_request());
    }
    let task = parser.non_empty_string("task")?.to_string();
    let background = parser.bool_or("background", false)?;
    let policies = resolve_sub_agent_policies(request, &mut parser)?;
    let returns_schema = sub_agent_returns_schema(&mut parser)?;
    let system = non_empty_raw_string(parser.opt_string("system")?.map(str::to_string));
    let session_id = non_empty_raw_string(parser.opt_string("session_id")?.map(str::to_string))
        .unwrap_or_else(|| format!("sub_agent_session_{}", uuid::Uuid::now_v7()));
    let mut options =
        prepare_sub_agent_options(&mut parser, &session_id, policies.requested_policy.as_ref())?;
    let name = non_empty_raw_string(parser.opt_string("name")?.map(str::to_string))
        .unwrap_or_else(|| "sub-agent".to_string());
    let (run_id, parent_session_id, parent_run_id) =
        lifecycle::initialize_run_identity(&mut options, &name);
    let reminder_propagation = match parser.opt_list("reminder_propagation")? {
        Some(reminders) => reminders
            .iter()
            .map(parse_reminder_propagation_value)
            .collect::<Result<Vec<_>, _>>()?,
        None => inherited_reminders_from_parent(parent_session_id.as_deref()),
    };
    let requested_workspace_anchor = parse_sub_agent_workspace_anchor(&mut parser)?;
    parser.finish(&[])?;
    let workspace_anchor = resolve_sub_agent_workspace_anchor(
        parent_session_id.as_deref(),
        requested_workspace_anchor,
    )?;
    let mut execution = policies.execution;
    default_sub_agent_execution_cwd(&mut execution, workspace_anchor.as_ref());

    Ok(ParsedSubAgentRequest {
        spec: SubAgentRunSpec {
            name,
            task,
            system,
            options,
            returns_schema,
            session_id,
            run_id,
            parent_session_id,
            parent_run_id,
            reminder_propagation,
            workspace_anchor,
            stop_emitted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        background,
        carry_policy: policies.carry_policy,
        execution,
        worker_policy: policies.worker_policy,
    })
}

fn parse_sub_agent_workspace_anchor(
    parser: &mut Options<'_, '_>,
) -> Result<Option<crate::workspace_anchor::WorkspaceAnchor>, VmError> {
    let Some(value) = parser.raw("anchor") else {
        return Ok(None);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    let mut anchor = crate::workspace_anchor::parse_anchor_dict(value)
        .map_err(|message| VmError::Runtime(format!("{SUB_AGENT_RUN_FN}: anchor: {message}")))?;
    anchor
        .canonicalize_roots()
        .map_err(|message| VmError::Runtime(format!("{SUB_AGENT_RUN_FN}: anchor: {message}")))?;
    Ok(Some(anchor))
}

fn resolve_sub_agent_workspace_anchor(
    parent_session_id: Option<&str>,
    requested: Option<crate::workspace_anchor::WorkspaceAnchor>,
) -> Result<Option<crate::workspace_anchor::WorkspaceAnchor>, VmError> {
    match requested {
        Some(anchor) => {
            validate_child_anchor_against_parent(parent_session_id, &anchor)?;
            Ok(Some(anchor))
        }
        None => Ok(parent_session_id.and_then(crate::agent_sessions::workspace_anchor)),
    }
}

fn default_sub_agent_execution_cwd(
    execution: &mut agents_workers::WorkerExecutionProfile,
    anchor: Option<&crate::workspace_anchor::WorkspaceAnchor>,
) {
    if execution
        .cwd
        .as_deref()
        .is_some_and(|cwd| !cwd.trim().is_empty())
    {
        return;
    }
    let Some(anchor) = anchor else {
        return;
    };
    execution.cwd = Some(anchor.primary.to_string_lossy().into_owned());
}

/// Reject child anchors that escape the parent's anchor + mounted
/// roots (#2223). The parent gates which directories sub-agents can
/// own; without this guard, a parent that itself runs scoped to /tmp/a
/// could spawn a child against /etc and silently widen its blast
/// radius.
fn validate_child_anchor_against_parent(
    parent_session_id: Option<&str>,
    child: &crate::workspace_anchor::WorkspaceAnchor,
) -> Result<(), VmError> {
    let Some(parent_session_id) = parent_session_id else {
        return Ok(());
    };
    let Some(parent) = crate::agent_sessions::workspace_anchor(parent_session_id) else {
        return Ok(());
    };
    let child_primary = child.primary.display().to_string();
    let modes = vec![
        crate::workspace_anchor::MountMode::ReadOnly,
        crate::workspace_anchor::MountMode::Extend,
        crate::workspace_anchor::MountMode::Sandboxed,
    ];
    if let Some(reason) =
        crate::llm::permissions::anchor_scope_violation(&child_primary, &parent, &modes)
    {
        return Err(VmError::Runtime(format!(
            "{SUB_AGENT_RUN_FN}: child anchor escapes parent: {reason}"
        )));
    }
    for root in &child.additional_roots {
        let root_path = root.path.display().to_string();
        if let Some(reason) =
            crate::llm::permissions::anchor_scope_violation(&root_path, &parent, &modes)
        {
            return Err(VmError::Runtime(format!(
                "{SUB_AGENT_RUN_FN}: child additional_root escapes parent: {reason}"
            )));
        }
    }
    Ok(())
}

fn validate_sub_agent_request_envelope(
    args: &[VmValue],
) -> Result<&crate::value::DictMap, VmError> {
    match args.first() {
        Some(VmValue::Dict(map)) => Ok(map.as_ref()),
        _ => Err(invalid_sub_agent_request()),
    }
}

fn invalid_sub_agent_request() -> VmError {
    VmError::Runtime(format!(
        "{SUB_AGENT_RUN_FN}: expected a normalized sub_agent_request dict"
    ))
}

#[cfg(test)]
#[path = "agents_sub_agent_tests.rs"]
mod tests;

fn resolve_sub_agent_policies(
    request: &crate::value::DictMap,
    parser: &mut Options<'_, '_>,
) -> Result<SubAgentPolicyResolution, VmError> {
    let allowed_tools =
        parse_string_list(parser.raw("allowed_tools"), "sub_agent_run.allowed_tools")?;
    let requested_policy = sub_agent_requested_policy(parser.raw("policy"), &allowed_tools)?;
    let worker_policy = agents_workers::resolve_inherited_worker_policy(requested_policy.clone())?;
    parser.allow("carry");
    let carry_policy = agents_workers::parse_worker_carry_policy(request)?;
    let execution = agents_workers::parse_worker_execution_profile(parser.raw("execution"))?;
    Ok(SubAgentPolicyResolution {
        requested_policy,
        worker_policy,
        carry_policy,
        execution,
    })
}

fn sub_agent_returns_schema(parser: &mut Options<'_, '_>) -> Result<Option<VmValue>, VmError> {
    let returns_schema_value = parser
        .raw("returns_schema")
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    let returns = parser.opt_dict("returns")?;
    Ok(returns_schema_value.or_else(|| returns.and_then(|dict| dict.get("schema")).cloned()))
}

fn prepare_sub_agent_options(
    parser: &mut Options<'_, '_>,
    session_id: &str,
    requested_policy: Option<&CapabilityPolicy>,
) -> Result<crate::value::DictMap, VmError> {
    let mut options = parser.opt_dict("options")?.cloned().unwrap_or_default();
    inject_sub_agent_skill_context(&mut options);
    options.put_str("session_id", session_id);
    match requested_policy {
        Some(policy) => {
            options.put("policy", super::to_vm(policy)?);
        }
        None => {
            options.remove("policy");
        }
    }
    Ok(options)
}

fn inject_sub_agent_skill_context(options: &mut crate::value::DictMap) {
    let Some(context) = crate::orchestration::current_workflow_stage_context() else {
        return;
    };
    if !options.contains_key("skills") {
        if let Some(registry) = context.registry {
            options.put("skills", registry);
        }
    }
    if !options.contains_key("skill_match") {
        if let Some(match_config) = context.match_config {
            options.put("skill_match", match_config);
        }
    }
}

fn sub_agent_error_dict(
    category: &str,
    message: impl Into<String>,
    tool: Option<String>,
) -> VmValue {
    let mut error = crate::value::DictMap::new();
    error.put_str("category", category);
    error.put_str("message", message.into());
    if let Some(tool) = tool {
        error.put_str("tool", tool);
    }
    VmValue::dict(error)
}

fn sub_agent_base_envelope(
    summary: String,
    artifacts: VmValue,
    evidence_added: i64,
    tokens_used: i64,
    budget_exceeded: bool,
    session_id: &str,
) -> crate::value::DictMap {
    let mut envelope = crate::value::DictMap::new();
    envelope.put("ok", VmValue::Bool(true));
    envelope.put_str("summary", summary);
    envelope.put("artifacts", artifacts);
    envelope.put("evidence_added", VmValue::Int(evidence_added));
    envelope.put("tokens_used", VmValue::Int(tokens_used));
    envelope.put("budget_exceeded", VmValue::Bool(budget_exceeded));
    envelope.put("data", VmValue::Nil);
    envelope.put("error", VmValue::Nil);
    envelope.put_str("session_id", session_id);
    // Per-child receipt fields (#29): the files this child actually mutated and
    // its token usage. `files_written` is DRAINED from the session's authoritative
    // hostlib write record (denied writes never reach it), so a failed child that
    // still wrote files is visible and a "claimed-done, zero writes" child is
    // detectable downstream. Present on BOTH the success and error envelope so the
    // parent fan-out report can reason about every child uniformly.
    envelope.insert(
        crate::value::intern_key("files_written"),
        crate::stdlib::json_to_vm_value(&serde_json::json!(
            crate::agent_sessions::take_session_changed_paths(session_id)
        )),
    );
    envelope.put(
        "usage",
        crate::stdlib::json_to_vm_value(&serde_json::json!({ "total_tokens": tokens_used })),
    );
    envelope
}

fn physical_result_usage(result: &serde_json::Value) -> Result<(i64, i64), VmError> {
    let usage = result.get("physical_usage").ok_or_else(|| {
        VmError::Runtime("agent_loop result is missing physical_usage".to_string())
    })?;
    let counter = |name: &str| {
        usage
            .get(name)
            .and_then(serde_json::Value::as_i64)
            .filter(|value| *value >= 0)
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "agent_loop physical_usage.{name} must be a non-negative integer"
                ))
            })
    };
    Ok((counter("input_tokens")?, counter("output_tokens")?))
}

pub(super) fn physical_usage_for_session(session_id: &str) -> Option<serde_json::Value> {
    crate::llm::agent_session_host::physical_usage_value_for_session(session_id)
        .as_ref()
        .map(crate::llm::vm_value_to_json)
}

struct SubAgentUsage<'a> {
    tokens_used: i64,
    physical_usage: &'a serde_json::Value,
}

impl SubAgentUsage<'_> {
    fn to_vm_value(&self) -> VmValue {
        let mut usage = self.physical_usage.clone();
        usage["total_tokens"] = serde_json::Value::from(self.tokens_used);
        crate::stdlib::json_to_vm_value(&usage)
    }
}

fn wrap_sub_agent_error(
    summary: String,
    artifacts: VmValue,
    evidence_added: i64,
    usage: &SubAgentUsage<'_>,
    budget_exceeded: bool,
    session_id: &str,
    error: VmValue,
    transcript: Option<VmValue>,
) -> VmValue {
    let mut envelope = sub_agent_base_envelope(
        summary,
        artifacts,
        evidence_added,
        usage.tokens_used,
        budget_exceeded,
        session_id,
    );
    envelope.put("ok", VmValue::Bool(false));
    envelope.put("usage", usage.to_vm_value());
    let status = error
        .as_dict()
        .and_then(|dict| dict.get("category"))
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "failed".to_string());
    envelope.put_str("status", status);
    envelope.put("error", error);
    if let Some(transcript) = transcript {
        envelope.put("transcript", transcript);
    }
    VmValue::dict(envelope)
}

fn append_parent_sub_agent_event(parent_session_id: Option<&str>, event: VmValue) {
    let Some(parent_session_id) = parent_session_id else {
        return;
    };
    if let Err(err) = crate::agent_sessions::append_event(parent_session_id, event) {
        crate::events::log_warn(
            "sub_agent_run.parent_event",
            &format!("parent_session_id={parent_session_id} child event append failed: {err}"),
        );
    }
}

fn seed_child_reminder_propagation(spec: &SubAgentRunSpec) -> Result<(), VmError> {
    for reminder in &spec.reminder_propagation {
        crate::agent_sessions::inject_reminder(&spec.session_id, reminder.clone())
            .map_err(VmError::Runtime)?;
        let mut payload =
            crate::llm::helpers::reminder_lifecycle_payload(Some(&spec.session_id), reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "originating_agent_id".to_string(),
                reminder
                    .originating_agent_id
                    .as_ref()
                    .map(|id| serde_json::Value::String(id.clone()))
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "sub_agent_id".to_string(),
                serde_json::Value::String(spec.session_id.clone()),
            );
        }
        crate::llm::helpers::emit_reminder_lifecycle_event(
            crate::llm::helpers::REMINDER_INHERITED_EVENT_KIND,
            payload,
        );
    }
    Ok(())
}

fn sub_agent_result_event(
    spec: &SubAgentRunSpec,
    ok: bool,
    summary: &str,
    evidence_added: i64,
    budget_exceeded: bool,
    error: Option<serde_json::Value>,
) -> VmValue {
    crate::llm::helpers::transcript_event(
        "sub_agent_result",
        "system",
        "internal",
        summary,
        Some(serde_json::json!({
            "name": spec.name,
            "child_session_id": spec.session_id,
            "ok": ok,
            "summary": summary,
            "evidence_added": evidence_added,
            "budget_exceeded": budget_exceeded,
            "error": error,
        })),
    )
}

fn permission_denied_from_transcript(transcript: &VmValue) -> Option<(String, String)> {
    let events = transcript
        .as_dict()
        .and_then(|dict| dict.get("events"))
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })?;
    for event in events.iter().rev() {
        let Some(dict) = event.as_dict() else {
            continue;
        };
        let kind = dict.get("kind").map(VmValue::display).unwrap_or_default();
        if kind == "PermissionDeny" {
            let metadata = dict.get("metadata").and_then(|v| v.as_dict());
            let tool = metadata
                .and_then(|m| m.get("tool_name"))
                .map(VmValue::display)
                .unwrap_or_default();
            let reason = metadata
                .and_then(|m| m.get("reason"))
                .map(VmValue::display)
                .unwrap_or_else(|| "permission denied".to_string());
            return Some((tool, reason));
        }
    }
    None
}

fn nested_budget_denial_error(result: &serde_json::Value) -> Option<VmValue> {
    if result.get("stop_reason").and_then(|value| value.as_str())
        != Some("nested_execution_budget_exhausted")
    {
        return None;
    }
    let error = result
        .get("error")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "category": "budget_exceeded",
                "kind": "budget_exhausted",
                "reason": "nested_execution_budget_exhausted",
                "message": "nested execution budget exhausted",
            })
        });
    Some(crate::stdlib::json_to_vm_value(&error))
}

#[derive(Clone, Debug)]
struct JsonCandidate {
    text: String,
    value: serde_json::Value,
}

#[derive(Clone, Debug, Default)]
struct TranscriptFallbacks {
    assistant_text: Option<String>,
    structured_json: Option<JsonCandidate>,
    summary: Option<String>,
}

#[derive(Clone, Debug)]
struct SynthesizedSubAgentResult {
    summary: String,
    structured_json: Option<JsonCandidate>,
}

fn extract_json_candidate(text: &str) -> Option<JsonCandidate> {
    let json = crate::stdlib::json::extract_json_from_text(text);
    let value = serde_json::from_str::<serde_json::Value>(&json).ok()?;
    Some(JsonCandidate {
        text: value.to_string(),
        value,
    })
}

fn transcript_assistant_message_text(message: &VmValue) -> Option<String> {
    let dict = message.as_dict()?;
    if dict.get("role").map(VmValue::display).as_deref() != Some("assistant") {
        return None;
    }
    match dict.get("content")? {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        VmValue::Dict(_) => Some(crate::llm::vm_value_to_json(dict.get("content")?).to_string()),
        _ => None,
    }
}

fn transcript_assistant_event_text(event: &VmValue) -> Option<String> {
    let dict = event.as_dict()?;
    if dict.get("role").map(VmValue::display).as_deref() != Some("assistant") {
        return None;
    }
    dict.get("text").and_then(|value| match value {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => None,
    })
}

fn normalized_assistant_text(text: &str) -> Option<String> {
    let sanitized = crate::visible_text::sanitize_visible_assistant_text(text, false);
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn collect_transcript_fallbacks(transcript: &VmValue) -> TranscriptFallbacks {
    let mut fallbacks = TranscriptFallbacks::default();
    let Some(dict) = transcript.as_dict() else {
        return fallbacks;
    };

    if let Some(VmValue::List(messages)) = dict.get("messages") {
        for message in messages.iter().rev() {
            let Some(text) = transcript_assistant_message_text(message) else {
                continue;
            };
            if fallbacks.structured_json.is_none() {
                fallbacks.structured_json = extract_json_candidate(&text);
            }
            if fallbacks.assistant_text.is_none() {
                fallbacks.assistant_text = normalized_assistant_text(&text);
            }
            if fallbacks.structured_json.is_some() && fallbacks.assistant_text.is_some() {
                break;
            }
        }
    }

    if (fallbacks.structured_json.is_none() || fallbacks.assistant_text.is_none())
        && matches!(dict.get("events"), Some(VmValue::List(_)))
    {
        if let Some(VmValue::List(events)) = dict.get("events") {
            for event in events.iter().rev() {
                let Some(text) = transcript_assistant_event_text(event) else {
                    continue;
                };
                if fallbacks.structured_json.is_none() {
                    fallbacks.structured_json = extract_json_candidate(&text);
                }
                if fallbacks.assistant_text.is_none() {
                    fallbacks.assistant_text = normalized_assistant_text(&text);
                }
                if fallbacks.structured_json.is_some() && fallbacks.assistant_text.is_some() {
                    break;
                }
            }
        }
    }

    fallbacks.summary = dict.get("summary").and_then(|value| match value {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => None,
    });
    if fallbacks.structured_json.is_none() {
        fallbacks.structured_json = fallbacks
            .summary
            .as_deref()
            .and_then(extract_json_candidate);
    }

    fallbacks
}

fn option_requests_structured_output(options: &crate::value::DictMap) -> bool {
    options.get("output").is_some_and(|value| match value {
        VmValue::Nil => false,
        VmValue::String(kind) => kind.as_str() != "text",
        _ => true,
    })
}

fn synthesize_sub_agent_result(
    result: &serde_json::Value,
    transcript: &VmValue,
    wants_structured_output: bool,
) -> SynthesizedSubAgentResult {
    let raw = result
        .get("visible_text")
        .and_then(|value| value.as_str())
        .or_else(|| result.get("text").and_then(|value| value.as_str()))
        .unwrap_or_default();
    let visible_text = crate::visible_text::sanitize_visible_assistant_text(raw, false);
    let visible_trimmed = visible_text.trim().to_string();
    let raw_trimmed = raw.trim().to_string();
    let direct_json = extract_json_candidate(if !visible_trimmed.is_empty() {
        &visible_trimmed
    } else {
        &raw_trimmed
    });

    let fallbacks = collect_transcript_fallbacks(transcript);
    let structured_json = direct_json.or_else(|| fallbacks.structured_json.clone());

    let summary = if wants_structured_output {
        structured_json
            .as_ref()
            .map(|candidate| candidate.text.clone())
            .or_else(|| (!visible_trimmed.is_empty()).then(|| visible_trimmed.clone()))
            .or_else(|| fallbacks.assistant_text.clone())
            .or_else(|| fallbacks.summary.clone())
            .unwrap_or(raw_trimmed)
    } else {
        (!visible_trimmed.is_empty())
            .then_some(visible_trimmed)
            .or_else(|| fallbacks.assistant_text.clone())
            .or_else(|| {
                fallbacks
                    .structured_json
                    .as_ref()
                    .map(|candidate| candidate.text.clone())
            })
            .or_else(|| fallbacks.summary.clone())
            .unwrap_or(raw_trimmed)
    };

    SynthesizedSubAgentResult {
        summary,
        structured_json,
    }
}

fn synthesize_sub_agent_declared_output(
    result: &serde_json::Value,
    transcript: &VmValue,
) -> SynthesizedSubAgentResult {
    // A declared schema describes the terminal output contract. Once that
    // contract was attempted, an earlier assistant JSON fragment is not a
    // substitute for its result. In particular, exhausted wrap-up output is
    // eligible only when the loop reports that its terminal schema check passed.
    let summary = synthesize_sub_agent_result(result, transcript, false).summary;
    let structured_json =
        if result.get("output_valid").and_then(|value| value.as_bool()) == Some(true) {
            result.get("output").map(|value| JsonCandidate {
                text: value.to_string(),
                value: value.clone(),
            })
        } else {
            None
        };
    let summary = structured_json
        .as_ref()
        .map(|candidate| candidate.text.clone())
        .unwrap_or(summary);
    SynthesizedSubAgentResult {
        summary,
        structured_json,
    }
}

fn declared_output_invalid(result: &serde_json::Value, wants_structured_output: bool) -> bool {
    wants_structured_output
        && result
            .get("output_valid")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
}

pub(super) async fn execute_sub_agent(
    ctx: &AsyncBuiltinCtx,
    mut spec: SubAgentRunSpec,
) -> Result<SubAgentExecutionResult, VmError> {
    normalize_run_identity(&mut spec);
    if let Some(parent_session_id) = spec.parent_session_id.as_deref() {
        crate::agent_sessions::open_child_session_with_actor(
            parent_session_id,
            Some(spec.session_id.clone()),
            Some(&spec.name),
        )
        .map_err(|error| VmError::Runtime(error.to_string()))?;
    } else {
        crate::agent_sessions::open_or_create(Some(spec.session_id.clone()))
            .map_err(|error| VmError::Runtime(error.to_string()))?;
    }
    if let Some(anchor) = spec.workspace_anchor.as_ref() {
        crate::agent_sessions::set_workspace_anchor(&spec.session_id, Some(anchor.clone()))
            .map_err(VmError::Runtime)?;
    }
    seed_child_reminder_propagation(&spec)?;
    append_parent_sub_agent_event(
        spec.parent_session_id.as_deref(),
        lifecycle::sub_agent_start_event(&spec),
    );

    let mut loop_options = spec.options.clone();
    loop_options.put_str("session_id", spec.session_id.clone());
    loop_options.put_str("run_id", spec.run_id.clone());
    let harness = ctx.child_vm().root_harness_value().ok_or_else(|| {
        VmError::Runtime("sub_agent_run: execution has no root Harness authority".to_string())
    })?;
    let args = vec![
        harness,
        VmValue::string(spec.task.clone()),
        spec.system
            .as_ref()
            .map(|system| VmValue::string(system.clone()))
            .unwrap_or(VmValue::Nil),
        VmValue::dict(loop_options),
    ];
    let result = crate::stdlib::harn_entry::call_harn_export_by_name(
        ctx,
        "std/agent/loop",
        "agent_loop",
        "sub_agent_run",
        &args,
    )
    .await;

    let (result, transcript) = match result {
        Ok(result_value) => {
            let result = crate::llm::vm_value_to_json(&result_value);
            let transcript_json = result.get("transcript").cloned().unwrap_or_default();
            (result, crate::stdlib::json_to_vm_value(&transcript_json))
        }
        Err(error) => return child_error::child_error_outcome(&spec, error),
    };
    let (input_tokens, output_tokens) = physical_result_usage(&result)?;
    let tokens_used = input_tokens.saturating_add(output_tokens);

    if result.get("status").and_then(|value| value.as_str()) == Some("suspended") {
        return Ok(SubAgentExecutionResult {
            payload: result,
            transcript,
            identity: crate::agent_events::AgentRunRef {
                session_id: spec.session_id.clone(),
                run_id: spec.run_id.clone(),
            },
        });
    }
    let wants_structured_output =
        spec.returns_schema.is_some() || option_requests_structured_output(&spec.options);
    let usage = SubAgentUsage {
        tokens_used,
        physical_usage: &result["physical_usage"],
    };
    let synthesized = if wants_structured_output {
        synthesize_sub_agent_declared_output(&result, &transcript)
    } else {
        synthesize_sub_agent_result(&result, &transcript, false)
    };
    let mut terminal_details = stop_details_for_result(&result);
    if terminal_details.status == crate::agent_events::SubagentTerminalStatus::Success {
        if declared_output_invalid(&result, wants_structured_output) {
            terminal_details = SubagentStopDetails::failure(
                "schema_validation",
                "sub-agent terminal output did not satisfy its declared output contract",
            );
        } else if result
            .get("publication_admitted")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            terminal_details = SubagentStopDetails::failure(
                "publication_denied",
                "sub-agent terminal result was not admitted for publication",
            );
        }
    }
    let summary = synthesized.summary.clone();
    let artifacts = transcript
        .as_dict()
        .and_then(|dict| dict.get("assets"))
        .cloned()
        .unwrap_or_else(|| VmValue::list(Vec::new()));
    let evidence_added = match &artifacts {
        VmValue::List(list) => list.len() as i64,
        _ => 0,
    };
    let budget_limit = spec
        .options
        .get("token_budget")
        .and_then(|value| value.as_int())
        .unwrap_or(-1);
    let nested_budget_error = nested_budget_denial_error(&result);
    let budget_exceeded =
        (budget_limit >= 0 && tokens_used >= budget_limit) || nested_budget_error.is_some();

    if let Some(error_value) = nested_budget_error {
        append_parent_sub_agent_event(
            spec.parent_session_id.as_deref(),
            sub_agent_result_event(
                &spec,
                false,
                &summary,
                evidence_added,
                true,
                Some(crate::llm::vm_value_to_json(&error_value)),
            ),
        );
        return Ok(finish_sub_agent(
            &spec,
            crate::llm::vm_value_to_json(&wrap_sub_agent_error(
                summary,
                artifacts,
                evidence_added,
                &usage,
                true,
                &spec.session_id,
                error_value,
                Some(transcript.clone()),
            )),
            transcript,
            SubagentStopDetails::failure("budget_exhausted", "nested budget was exhausted"),
        ));
    }

    let mut envelope = sub_agent_base_envelope(
        summary.clone(),
        artifacts.clone(),
        evidence_added,
        tokens_used,
        budget_exceeded,
        &spec.session_id,
    );
    envelope.put("transcript", transcript.clone());
    // Preserve the finalized per-child physical usage ledger, including its
    // scope and accounting certainty, in addition to the base total.
    envelope.put("usage", usage.to_vm_value());

    if wants_structured_output {
        if let Some(candidate) = synthesized.structured_json.as_ref() {
            envelope.put("data", crate::stdlib::json_to_vm_value(&candidate.value));
        }
    }

    if let Some((tool, reason)) = permission_denied_from_transcript(&transcript) {
        append_parent_sub_agent_event(
            spec.parent_session_id.as_deref(),
            sub_agent_result_event(
                &spec,
                false,
                &summary,
                evidence_added,
                budget_exceeded,
                Some(crate::llm::vm_value_to_json(&sub_agent_error_dict(
                    "permission_denied",
                    reason.clone(),
                    Some(tool.clone()),
                ))),
            ),
        );
        return Ok(finish_sub_agent(
            &spec,
            crate::llm::vm_value_to_json(&wrap_sub_agent_error(
                summary,
                artifacts,
                evidence_added,
                &usage,
                budget_exceeded,
                &spec.session_id,
                sub_agent_error_dict("permission_denied", reason, Some(tool)),
                Some(transcript.clone()),
            )),
            transcript,
            SubagentStopDetails::failure("permission_denied", "sub-agent tool permission denied"),
        ));
    }

    let terminal_ok =
        terminal_details.status == crate::agent_events::SubagentTerminalStatus::Success;
    let terminal_error = if terminal_ok {
        None
    } else {
        Some(sub_agent_error_dict(
            &terminal_details.terminal_class,
            terminal_details.reason.clone(),
            None,
        ))
    };
    envelope.put("ok", VmValue::Bool(terminal_ok));
    envelope.put_str(
        "status",
        if terminal_ok
            || result
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|status| !status.is_empty() && status != "done")
        {
            result
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("done")
        } else {
            terminal_details.terminal_class.as_str()
        },
    );
    if let Some(error) = terminal_error.as_ref() {
        envelope.put("error", error.clone());
    }

    append_parent_sub_agent_event(
        spec.parent_session_id.as_deref(),
        sub_agent_result_event(
            &spec,
            terminal_ok,
            &synthesized.summary,
            evidence_added,
            budget_exceeded,
            terminal_error.as_ref().map(crate::llm::vm_value_to_json),
        ),
    );

    Ok(finish_sub_agent(
        &spec,
        crate::llm::vm_value_to_json(&VmValue::dict(envelope)),
        transcript,
        terminal_details,
    ))
}
