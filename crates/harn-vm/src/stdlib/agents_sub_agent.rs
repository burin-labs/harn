use crate::value::VmDictExt;

use super::agents_workers;
use super::{SubAgentExecutionResult, SubAgentRunSpec};
use crate::orchestration::{
    annotate_nested_execution_options, CapabilityPolicy, NestedExecutionKind,
};
use crate::stdlib::options::{ErrorKind, OptionsParser};
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

const SUB_AGENT_RUN_FN: &str = "sub_agent_run";

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

fn parse_string_list(value: Option<&VmValue>, label: &str) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let VmValue::List(list) = value else {
        return Err(VmError::Runtime(format!(
            "{label}: expected a list of strings"
        )));
    };
    let mut values = Vec::new();
    for item in list.iter() {
        let VmValue::String(text) = item else {
            return Err(VmError::Runtime(format!(
                "{label}: expected a list of strings"
            )));
        };
        let text = text.trim();
        if !text.is_empty() && !values.iter().any(|existing| existing == text) {
            values.push(text.to_string());
        }
    }
    Ok(values)
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
    let mut parser = OptionsParser::new(SUB_AGENT_RUN_FN, request, ErrorKind::Runtime);
    if parser.optional_string_raw("_type")?.as_deref() != Some("sub_agent_request") {
        return Err(invalid_sub_agent_request());
    }
    let task = parser.required_string("task")?;
    let background = parser.bool_or("background", false)?;
    let policies = resolve_sub_agent_policies(request, &mut parser)?;
    let returns_schema = sub_agent_returns_schema(&mut parser)?;
    let system = non_empty_raw_string(parser.optional_string_raw("system")?);
    let session_id = non_empty_raw_string(parser.optional_string_raw("session_id")?)
        .unwrap_or_else(|| format!("sub_agent_session_{}", uuid::Uuid::now_v7()));
    let mut options =
        prepare_sub_agent_options(&mut parser, &session_id, policies.requested_policy.as_ref())?;
    let name = non_empty_raw_string(parser.optional_string_raw("name")?)
        .unwrap_or_else(|| "sub-agent".to_string());
    annotate_nested_execution_options(&mut options, NestedExecutionKind::SubAgentRun, &name);
    let parent_session_id = crate::llm::current_agent_session_id();
    let reminder_propagation = match parser.optional_list("reminder_propagation")? {
        Some(reminders) => reminders
            .iter()
            .map(parse_reminder_propagation_value)
            .collect::<Result<Vec<_>, _>>()?,
        None => inherited_reminders_from_parent(parent_session_id.as_deref()),
    };
    let requested_workspace_anchor = parse_sub_agent_workspace_anchor(&mut parser)?;
    parser.finish_strict(&[])?;
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
            parent_session_id,
            reminder_propagation,
            workspace_anchor,
        },
        background,
        carry_policy: policies.carry_policy,
        execution,
        worker_policy: policies.worker_policy,
    })
}

fn parse_sub_agent_workspace_anchor(
    parser: &mut OptionsParser<'_>,
) -> Result<Option<crate::workspace_anchor::WorkspaceAnchor>, VmError> {
    let Some(value) = parser.raw("anchor") else {
        return Ok(None);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    let anchor = crate::workspace_anchor::parse_anchor_dict(value)
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

fn resolve_sub_agent_policies(
    request: &crate::value::DictMap,
    parser: &mut OptionsParser<'_>,
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

fn sub_agent_returns_schema(parser: &mut OptionsParser<'_>) -> Result<Option<VmValue>, VmError> {
    let returns_schema_value = parser
        .raw("returns_schema")
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    let returns = parser.optional_dict("returns")?;
    Ok(returns_schema_value.or_else(|| returns.and_then(|dict| dict.get("schema")).cloned()))
}

fn prepare_sub_agent_options(
    parser: &mut OptionsParser<'_>,
    session_id: &str,
    requested_policy: Option<&CapabilityPolicy>,
) -> Result<crate::value::DictMap, VmError> {
    let mut options = parser
        .optional_dict("options")?
        .cloned()
        .unwrap_or_default();
    inject_sub_agent_skill_context(&mut options);
    options.put_str("session_id", session_id);
    match requested_policy {
        Some(policy) => {
            options.insert(crate::value::intern_key("policy"), super::to_vm(policy)?);
        }
        None => {
            options.remove("policy");
        }
    }
    Ok(options)
}

fn inject_sub_agent_skill_context(options: &mut crate::value::DictMap) {
    let Some(context) = crate::orchestration::current_workflow_skill_context() else {
        return;
    };
    if !options.contains_key("skills") {
        if let Some(registry) = context.registry {
            options.insert(crate::value::intern_key("skills"), registry);
        }
    }
    if !options.contains_key("skill_match") {
        if let Some(match_config) = context.match_config {
            options.insert(crate::value::intern_key("skill_match"), match_config);
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
    envelope.insert(crate::value::intern_key("ok"), VmValue::Bool(true));
    envelope.put_str("summary", summary);
    envelope.insert(crate::value::intern_key("artifacts"), artifacts);
    envelope.insert(
        crate::value::intern_key("evidence_added"),
        VmValue::Int(evidence_added),
    );
    envelope.insert(
        crate::value::intern_key("tokens_used"),
        VmValue::Int(tokens_used),
    );
    envelope.insert(
        crate::value::intern_key("budget_exceeded"),
        VmValue::Bool(budget_exceeded),
    );
    envelope.insert(crate::value::intern_key("data"), VmValue::Nil);
    envelope.insert(crate::value::intern_key("error"), VmValue::Nil);
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
    envelope.insert(
        crate::value::intern_key("usage"),
        crate::stdlib::json_to_vm_value(&serde_json::json!({ "total_tokens": tokens_used })),
    );
    envelope
}

/// Split a child transcript's token usage into (input, output). Mirrors
/// [`transcript_tokens_used`] (whose total is `input + output`) but keeps the two
/// halves so the receipt can report `tokens_in` / `tokens_out` separately.
fn transcript_usage(transcript: &VmValue) -> (i64, i64) {
    let Some(events) = transcript
        .as_dict()
        .and_then(|dict| dict.get("events"))
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
    else {
        return (0, 0);
    };
    let mut input = 0i64;
    let mut output = 0i64;
    for metadata in events
        .iter()
        .filter_map(|event| event.as_dict())
        .filter_map(|dict| dict.get("metadata").and_then(|value| value.as_dict()))
    {
        input = input.saturating_add(
            metadata
                .get("input_tokens")
                .and_then(VmValue::as_int)
                .unwrap_or(0),
        );
        output = output.saturating_add(
            metadata
                .get("output_tokens")
                .and_then(VmValue::as_int)
                .unwrap_or(0),
        );
    }
    (input, output)
}

fn wrap_sub_agent_error(
    summary: String,
    artifacts: VmValue,
    evidence_added: i64,
    tokens_used: i64,
    budget_exceeded: bool,
    session_id: &str,
    error: VmValue,
    transcript: Option<VmValue>,
) -> VmValue {
    let mut envelope = sub_agent_base_envelope(
        summary,
        artifacts,
        evidence_added,
        tokens_used,
        budget_exceeded,
        session_id,
    );
    envelope.insert(crate::value::intern_key("ok"), VmValue::Bool(false));
    envelope.insert(crate::value::intern_key("error"), error);
    if let Some(transcript) = transcript {
        envelope.insert(crate::value::intern_key("transcript"), transcript);
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

fn sub_agent_start_event(spec: &SubAgentRunSpec) -> VmValue {
    crate::llm::helpers::transcript_event(
        "sub_agent_start",
        "system",
        "internal",
        &spec.task,
        Some(serde_json::json!({
            "name": spec.name,
            "child_session_id": spec.session_id,
            "task": spec.task,
        })),
    )
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

fn transcript_tokens_used(transcript: &VmValue) -> i64 {
    transcript
        .as_dict()
        .and_then(|dict| dict.get("events"))
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.as_dict())
                .filter_map(|dict| dict.get("metadata").and_then(|value| value.as_dict()))
                .map(|metadata| {
                    metadata
                        .get("input_tokens")
                        .and_then(VmValue::as_int)
                        .unwrap_or(0)
                        .saturating_add(
                            metadata
                                .get("output_tokens")
                                .and_then(VmValue::as_int)
                                .unwrap_or(0),
                        )
                })
                .sum()
        })
        .unwrap_or(0)
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
    matches!(
        options.get("response_format"),
        Some(VmValue::String(value)) if value.as_str() == "json"
    ) || options.contains_key("output_format")
        || options.contains_key("json_schema")
        || options.contains_key("output_schema")
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

fn parse_structured_sub_agent_data(
    candidate: Option<&JsonCandidate>,
    schema: &VmValue,
) -> Result<VmValue, VmError> {
    let Some(candidate) = candidate else {
        return Err(VmError::CategorizedError {
            message: "sub_agent_run: child transcript did not contain valid JSON".to_string(),
            category: crate::value::ErrorCategory::SchemaValidation,
        });
    };
    crate::schema::schema_expect_value(
        &crate::stdlib::json_to_vm_value(&candidate.value),
        schema,
        false,
    )
    .map_err(|error| match error {
        VmError::Thrown(VmValue::String(message)) => VmError::CategorizedError {
            message: format!("sub_agent_run: return schema validation failed: {message}"),
            category: crate::value::ErrorCategory::SchemaValidation,
        },
        other => other,
    })
}

pub(super) async fn execute_sub_agent(
    ctx: &AsyncBuiltinCtx,
    spec: SubAgentRunSpec,
) -> Result<SubAgentExecutionResult, VmError> {
    if let Some(parent_session_id) = spec.parent_session_id.as_deref() {
        crate::agent_sessions::open_child_session_with_actor(
            parent_session_id,
            Some(spec.session_id.clone()),
            Some(&spec.name),
        );
    } else {
        crate::agent_sessions::open_or_create(Some(spec.session_id.clone()));
    }
    if let Some(anchor) = spec.workspace_anchor.as_ref() {
        crate::agent_sessions::set_workspace_anchor(&spec.session_id, Some(anchor.clone()))
            .map_err(VmError::Runtime)?;
    }
    seed_child_reminder_propagation(&spec)?;
    append_parent_sub_agent_event(
        spec.parent_session_id.as_deref(),
        sub_agent_start_event(&spec),
    );

    let mut loop_options = spec.options.clone();
    loop_options.put_str("session_id", spec.session_id.clone());
    let args = vec![
        VmValue::String(arcstr::ArcStr::from(spec.task.clone())),
        spec.system
            .as_ref()
            .map(|system| VmValue::String(arcstr::ArcStr::from(system.clone())))
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
        Err(error) => {
            let error_value = match &error {
                VmError::CategorizedError { message, category } => {
                    sub_agent_error_dict(category.as_str(), message.clone(), None)
                }
                VmError::Thrown(VmValue::String(message)) => {
                    sub_agent_error_dict("runtime", message.to_string(), None)
                }
                _ => sub_agent_error_dict(
                    crate::value::error_to_category(&error).as_str(),
                    error.to_string(),
                    None,
                ),
            };
            let transcript = crate::agent_sessions::transcript(&spec.session_id)
                .unwrap_or_else(|| crate::stdlib::json_to_vm_value(&serde_json::json!({})));
            let tokens_used = transcript_tokens_used(&transcript);
            let envelope = wrap_sub_agent_error(
                String::new(),
                VmValue::List(std::sync::Arc::new(Vec::new())),
                0,
                tokens_used,
                false,
                &spec.session_id,
                error_value.clone(),
                Some(transcript.clone()),
            );
            append_parent_sub_agent_event(
                spec.parent_session_id.as_deref(),
                sub_agent_result_event(
                    &spec,
                    false,
                    "",
                    0,
                    false,
                    Some(crate::llm::vm_value_to_json(&error_value)),
                ),
            );
            return Ok(SubAgentExecutionResult {
                payload: crate::llm::vm_value_to_json(&envelope),
                transcript,
            });
        }
    };
    let tokens_used = transcript_tokens_used(&transcript);

    if result.get("status").and_then(|value| value.as_str()) == Some("suspended") {
        return Ok(SubAgentExecutionResult {
            payload: result,
            transcript,
        });
    }

    let wants_structured_output =
        spec.returns_schema.is_some() || option_requests_structured_output(&spec.options);
    let synthesized = synthesize_sub_agent_result(&result, &transcript, wants_structured_output);
    let summary = synthesized.summary.clone();
    let artifacts = transcript
        .as_dict()
        .and_then(|dict| dict.get("assets"))
        .cloned()
        .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new())));
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
        return Ok(SubAgentExecutionResult {
            payload: crate::llm::vm_value_to_json(&wrap_sub_agent_error(
                summary,
                artifacts,
                evidence_added,
                tokens_used,
                true,
                &spec.session_id,
                error_value,
                Some(transcript.clone()),
            )),
            transcript,
        });
    }

    let mut envelope = sub_agent_base_envelope(
        summary.clone(),
        artifacts.clone(),
        evidence_added,
        tokens_used,
        budget_exceeded,
        &spec.session_id,
    );
    envelope.insert(crate::value::intern_key("transcript"), transcript.clone());
    // Enrich the receipt's `usage` with the input/output split now that a full
    // transcript is in hand (the base envelope carries total-only).
    let (input_tokens, output_tokens) = transcript_usage(&transcript);
    envelope.insert(
        crate::value::intern_key("usage"),
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": tokens_used,
        })),
    );

    if spec.returns_schema.is_none() && option_requests_structured_output(&spec.options) {
        if let Some(candidate) = synthesized.structured_json.as_ref() {
            envelope.insert(
                crate::value::intern_key("data"),
                crate::stdlib::json_to_vm_value(&candidate.value),
            );
        }
    }

    if let Some(schema) = spec.returns_schema.as_ref() {
        match parse_structured_sub_agent_data(synthesized.structured_json.as_ref(), schema) {
            Ok(data) => {
                envelope.insert(crate::value::intern_key("data"), data);
            }
            Err(error) => {
                let message = error.to_string();
                append_parent_sub_agent_event(
                    spec.parent_session_id.as_deref(),
                    sub_agent_result_event(
                        &spec,
                        false,
                        &summary,
                        evidence_added,
                        budget_exceeded,
                        Some(crate::llm::vm_value_to_json(&sub_agent_error_dict(
                            crate::value::error_to_category(&error).as_str(),
                            message.clone(),
                            None,
                        ))),
                    ),
                );
                return Ok(SubAgentExecutionResult {
                    payload: crate::llm::vm_value_to_json(&wrap_sub_agent_error(
                        summary,
                        artifacts,
                        evidence_added,
                        tokens_used,
                        budget_exceeded,
                        &spec.session_id,
                        sub_agent_error_dict(
                            crate::value::error_to_category(&error).as_str(),
                            message,
                            None,
                        ),
                        Some(transcript.clone()),
                    )),
                    transcript,
                });
            }
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
        return Ok(SubAgentExecutionResult {
            payload: crate::llm::vm_value_to_json(&wrap_sub_agent_error(
                summary,
                artifacts,
                evidence_added,
                tokens_used,
                budget_exceeded,
                &spec.session_id,
                sub_agent_error_dict("permission_denied", reason, Some(tool)),
                Some(transcript.clone()),
            )),
            transcript,
        });
    }

    append_parent_sub_agent_event(
        spec.parent_session_id.as_deref(),
        sub_agent_result_event(
            &spec,
            true,
            &synthesized.summary,
            evidence_added,
            budget_exceeded,
            None,
        ),
    );

    Ok(SubAgentExecutionResult {
        payload: crate::llm::vm_value_to_json(&VmValue::dict(envelope)),
        transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::{push_llm_mock, reset_llm_mock_state, LlmMock};

    struct ExecutionPolicyGuard;

    impl Drop for ExecutionPolicyGuard {
        fn drop(&mut self) {
            crate::orchestration::clear_execution_policy_stacks();
        }
    }

    fn push_recursion_policy(limit: usize) -> ExecutionPolicyGuard {
        crate::orchestration::clear_execution_policy_stacks();
        crate::orchestration::push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(limit),
            ..CapabilityPolicy::default()
        });
        ExecutionPolicyGuard
    }

    #[test]
    fn base_envelope_carries_files_written_and_usage_then_drains() {
        let session = format!("test-files-written-{}", uuid::Uuid::now_v7());
        crate::agent_sessions::clear_session_changed_paths(&session);
        crate::agent_sessions::record_session_changed_path(&session, "src/alpha.rs");
        crate::agent_sessions::record_session_changed_path(&session, "src/beta.rs");

        let envelope = sub_agent_base_envelope(
            "did the work".to_string(),
            VmValue::List(std::sync::Arc::new(Vec::new())),
            0,
            1234,
            false,
            &session,
        );

        let files = envelope
            .get("files_written")
            .and_then(|value| match value {
                VmValue::List(list) => Some(list.clone()),
                _ => None,
            })
            .expect("files_written is a list");
        let paths: Vec<String> = files
            .iter()
            .filter_map(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            })
            .collect();
        assert!(
            paths.contains(&"src/alpha.rs".to_string()),
            "alpha written: {paths:?}"
        );
        assert!(
            paths.contains(&"src/beta.rs".to_string()),
            "beta written: {paths:?}"
        );

        let total = envelope
            .get("usage")
            .and_then(VmValue::as_dict)
            .and_then(|usage| usage.get("total_tokens"))
            .and_then(VmValue::as_int)
            .expect("usage.total_tokens present");
        assert_eq!(total, 1234, "usage carries the child's total tokens");

        // The base envelope DRAINS the session record, so a second build sees no
        // double-counted writes.
        let again = sub_agent_base_envelope(
            "again".to_string(),
            VmValue::List(std::sync::Arc::new(Vec::new())),
            0,
            0,
            false,
            &session,
        );
        let again_files = again.get("files_written").and_then(|value| match value {
            VmValue::List(list) => Some(list.len()),
            _ => None,
        });
        assert_eq!(
            again_files,
            Some(0),
            "files_written drains after the first build"
        );
    }

    fn assistant_message(text: &str) -> VmValue {
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("role"),
                VmValue::String(arcstr::ArcStr::from("assistant")),
            ),
            (
                crate::value::intern_key("content"),
                VmValue::String(arcstr::ArcStr::from(text)),
            ),
        ]))
    }

    fn normalized_request(extra: Vec<(&str, VmValue)>) -> VmValue {
        let mut request = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("_type"),
                VmValue::String(arcstr::ArcStr::from("sub_agent_request")),
            ),
            (
                crate::value::intern_key("task"),
                VmValue::String(arcstr::ArcStr::from("summarize")),
            ),
        ]);
        for (key, value) in extra {
            request.insert(crate::value::intern_key(key), value);
        }
        VmValue::dict(request)
    }

    #[test]
    fn parse_sub_agent_request_rejects_unknown_top_level_options() {
        let request = normalized_request(vec![("backgrund", VmValue::Bool(true))]);

        let err = match parse_sub_agent_request(&[request]) {
            Ok(_) => panic!("expected unknown option failure"),
            Err(err) => err,
        };

        match err {
            VmError::Runtime(message) => assert!(message.contains("backgrund"), "got: {message}"),
            other => panic!("expected Runtime error, got {other:?}"),
        }
    }

    #[test]
    fn parse_sub_agent_request_preserves_session_id_whitespace() {
        let request = normalized_request(vec![(
            "session_id",
            VmValue::String(arcstr::ArcStr::from("  child-session  ")),
        )]);

        let parsed = parse_sub_agent_request(&[request]).unwrap();

        assert_eq!(parsed.spec.session_id, "  child-session  ");
    }

    fn anchor_dict(primary: &str, additional: Vec<(&str, &str)>) -> VmValue {
        let mut roots = Vec::new();
        for (path, mount_mode) in additional {
            roots.push(VmValue::dict(crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("path"),
                    VmValue::String(arcstr::ArcStr::from(path)),
                ),
                (
                    crate::value::intern_key("mount_mode"),
                    VmValue::String(arcstr::ArcStr::from(mount_mode)),
                ),
                (
                    crate::value::intern_key("mounted_at"),
                    VmValue::String(arcstr::ArcStr::from("2026-05-24T00:00:00Z")),
                ),
            ])));
        }
        let mut anchor = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("primary"),
                VmValue::String(arcstr::ArcStr::from(primary)),
            ),
            (
                crate::value::intern_key("anchored_at"),
                VmValue::String(arcstr::ArcStr::from("2026-05-24T00:00:00Z")),
            ),
        ]);
        if !roots.is_empty() {
            anchor.insert(
                crate::value::intern_key("additional_roots"),
                VmValue::List(std::sync::Arc::new(roots)),
            );
        }
        VmValue::dict(anchor)
    }

    fn path_string(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn parse_sub_agent_request_inherits_parent_anchor_as_execution_cwd() {
        crate::agent_sessions::reset_session_store();
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        let project_text = path_string(&project);
        let parent_id =
            crate::agent_sessions::open_or_create(Some("anchor-inherit-parent".to_string()));
        crate::agent_sessions::set_workspace_anchor(
            &parent_id,
            Some(crate::workspace_anchor::WorkspaceAnchor {
                primary: project.clone(),
                additional_roots: Vec::new(),
                anchored_at: "2026-05-24T00:00:00Z".to_string(),
            }),
        )
        .unwrap();
        let _guard = crate::agent_sessions::enter_current_session(parent_id.clone());

        let parsed = parse_sub_agent_request(&[normalized_request(Vec::new())]).unwrap();

        assert_eq!(
            parsed.spec.parent_session_id.as_deref(),
            Some(parent_id.as_str())
        );
        assert_eq!(
            parsed.spec.workspace_anchor.as_ref().unwrap().primary,
            project
        );
        assert_eq!(
            parsed.execution.cwd.as_deref(),
            Some(project_text.as_str()),
            "unanchored child workers default their sandbox execution cwd to the parent workspace"
        );
    }

    #[test]
    fn parse_sub_agent_request_preserves_explicit_execution_cwd() {
        crate::agent_sessions::reset_session_store();
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        let child = project.join("child");
        let explicit_cwd = dir.path().join("explicit-cwd");
        let explicit_cwd_text = path_string(&explicit_cwd);
        let parent_id =
            crate::agent_sessions::open_or_create(Some("anchor-explicit-cwd-parent".to_string()));
        crate::agent_sessions::set_workspace_anchor(
            &parent_id,
            Some(crate::workspace_anchor::WorkspaceAnchor {
                primary: project,
                additional_roots: Vec::new(),
                anchored_at: "2026-05-24T00:00:00Z".to_string(),
            }),
        )
        .unwrap();
        let _guard = crate::agent_sessions::enter_current_session(parent_id);

        let parsed = parse_sub_agent_request(&[normalized_request(vec![
            ("anchor", anchor_dict(&path_string(&child), Vec::new())),
            (
                "execution",
                VmValue::dict(crate::value::DictMap::from_iter([(
                    crate::value::intern_key("cwd"),
                    VmValue::String(arcstr::ArcStr::from(explicit_cwd_text.clone())),
                )])),
            ),
        ])])
        .unwrap();

        assert_eq!(
            parsed.spec.workspace_anchor.as_ref().unwrap().primary,
            child
        );
        assert_eq!(
            parsed.execution.cwd.as_deref(),
            Some(explicit_cwd_text.as_str()),
            "explicit execution.cwd remains authoritative"
        );
    }

    #[test]
    fn parse_sub_agent_request_accepts_anchor_in_parent_scope() {
        crate::agent_sessions::reset_session_store();
        let dir = tempfile::tempdir().expect("tempdir");
        let parent = dir.path().join("parent");
        let sibling = dir.path().join("sibling");
        let parent_nested = parent.join("nested");
        let sibling_sub = sibling.join("sub");
        let parent_nested_text = path_string(&parent_nested);
        let sibling_sub_text = path_string(&sibling_sub);
        let parent_id = crate::agent_sessions::open_or_create(Some("anchor-parent".to_string()));
        crate::agent_sessions::set_workspace_anchor(
            &parent_id,
            Some(crate::workspace_anchor::WorkspaceAnchor {
                primary: parent,
                additional_roots: vec![crate::workspace_anchor::MountedRoot {
                    path: sibling,
                    mount_mode: crate::workspace_anchor::MountMode::Extend,
                    mounted_at: "2026-05-24T00:00:00Z".to_string(),
                }],
                anchored_at: "2026-05-24T00:00:00Z".to_string(),
            }),
        )
        .unwrap();
        let _guard = crate::agent_sessions::enter_current_session(parent_id);

        for primary in [&parent_nested_text, &sibling_sub_text] {
            let request = normalized_request(vec![("anchor", anchor_dict(primary, Vec::new()))]);
            let parsed = parse_sub_agent_request(&[request])
                .unwrap_or_else(|err| panic!("anchor {primary} should be accepted: {err}"));
            assert_eq!(
                parsed.spec.workspace_anchor.as_ref().unwrap().primary,
                std::path::PathBuf::from(primary)
            );
        }
    }

    #[test]
    fn parse_sub_agent_request_rejects_anchor_outside_parent_scope() {
        crate::agent_sessions::reset_session_store();
        let dir = tempfile::tempdir().expect("tempdir");
        let parent = dir.path().join("parent");
        let outside = dir.path().join("elsewhere");
        let outside_text = path_string(&outside);
        let parent_id = crate::agent_sessions::open_or_create(Some("anchor-parent".to_string()));
        crate::agent_sessions::set_workspace_anchor(
            &parent_id,
            Some(crate::workspace_anchor::WorkspaceAnchor {
                primary: parent,
                additional_roots: Vec::new(),
                anchored_at: "2026-05-24T00:00:00Z".to_string(),
            }),
        )
        .unwrap();
        let _guard = crate::agent_sessions::enter_current_session(parent_id);

        let request = normalized_request(vec![("anchor", anchor_dict(&outside_text, Vec::new()))]);
        let err = parse_sub_agent_request(&[request])
            .err()
            .expect("expected anchor escape to fail");
        match err {
            VmError::Runtime(message) => {
                assert!(
                    message.contains("child anchor escapes parent"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected Runtime error, got {other:?}"),
        }
    }

    #[test]
    fn parse_sub_agent_request_anchor_without_parent_passes() {
        crate::agent_sessions::reset_session_store();
        let request = normalized_request(vec![(
            "anchor",
            anchor_dict("/workspace/anywhere", Vec::new()),
        )]);
        let parsed = parse_sub_agent_request(&[request]).unwrap();
        assert_eq!(
            parsed.spec.workspace_anchor.as_ref().unwrap().primary,
            std::path::PathBuf::from("/workspace/anywhere")
        );
    }

    #[test]
    fn synthesize_summary_uses_prior_assistant_json_from_transcript() {
        let transcript = crate::llm::helpers::new_transcript_with(
            None,
            vec![
                assistant_message("{\"answer\":\"ok\"}"),
                assistant_message("##DONE##"),
            ],
            None,
            None,
        );
        let result = serde_json::json!({
            "visible_text": "##DONE##",
            "text": "##DONE##",
        });

        let synthesized = synthesize_sub_agent_result(&result, &transcript, true);

        assert_eq!(synthesized.summary, "{\"answer\":\"ok\"}");
        assert_eq!(
            synthesized
                .structured_json
                .as_ref()
                .and_then(|candidate| candidate.value.get("answer"))
                .and_then(|value| value.as_str()),
            Some("ok")
        );
    }

    #[test]
    fn synthesize_summary_falls_back_to_assistant_event_history() {
        let transcript = crate::llm::helpers::new_transcript_with_events(
            None,
            Vec::new(),
            None,
            None,
            vec![crate::llm::helpers::transcript_event(
                "message",
                "assistant",
                "public",
                "{\"paths\":[\"src/lib.rs\"]}",
                None,
            )],
            Vec::new(),
            Some("active"),
        );
        let result = serde_json::json!({
            "visible_text": "",
            "text": "",
        });

        let synthesized = synthesize_sub_agent_result(&result, &transcript, true);

        assert_eq!(synthesized.summary, "{\"paths\":[\"src/lib.rs\"]}");
        assert_eq!(
            synthesized
                .structured_json
                .as_ref()
                .and_then(|candidate| candidate.value.get("paths"))
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_sub_agent_uses_child_transcript_and_appends_parent_events() {
        crate::agent_sessions::reset_session_store();
        reset_llm_mock_state();
        let parent = crate::agent_sessions::open_or_create(Some("parent-subagent".into()));
        let parent_chain = crate::ActorChain::new("user:kenneth").pushed("agent:root");
        crate::agent_sessions::set_actor_chain(&parent, Some(parent_chain)).unwrap();
        crate::agent_sessions::inject_message(&parent, assistant_message("parent context"))
            .unwrap();
        crate::agent_sessions::claim_tool_format(&parent, "text").unwrap();
        push_llm_mock(LlmMock {
            text: "child result".to_string(),
            tool_calls: Vec::new(),
            raw_tool_calls: Vec::new(),
            match_pattern: None,
            scope: crate::llm::mock::DEFAULT_MOCK_SCOPE.to_string(),
            entry_id: String::new(),
            sticky: false,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            model: "mock".to_string(),
            provider: None,
            blocks: None,
            logprobs: Vec::new(),
            error: None,
            stream_chunks: Vec::new(),
        });
        let spec = SubAgentRunSpec {
            name: "research-worker".to_string(),
            task: "inspect the repo".to_string(),
            system: None,
            options: crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("provider"),
                    VmValue::String(arcstr::ArcStr::from("mock")),
                ),
                (
                    crate::value::intern_key("model"),
                    VmValue::String(arcstr::ArcStr::from("mock")),
                ),
                (crate::value::intern_key("max_iterations"), VmValue::Int(1)),
            ]),
            returns_schema: None,
            session_id: "child-subagent".to_string(),
            parent_session_id: Some(parent.clone()),
            reminder_propagation: Vec::new(),
            workspace_anchor: None,
        };

        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
        let result = execute_sub_agent(&ctx, spec).await.unwrap();
        assert_eq!(result.payload["ok"].as_bool(), Some(true));

        let child_messages = crate::agent_sessions::messages_json("child-subagent");
        assert!(!child_messages
            .iter()
            .any(|message| message["content"].as_str() == Some("parent context")));
        // The child sub-agent resolves its OWN tool_format default — the spec
        // pins none, and `mock`/`mock` has no capability pin, so it lands on the
        // global text-channel default, which is now fenced-json (`json`), not
        // heredoc (`text`). (The parent's separate `text` claim does not bleed
        // into the child; the child always resolved its own default here.)
        assert_eq!(
            crate::agent_sessions::tool_format("child-subagent").as_deref(),
            Some("json")
        );
        assert_eq!(
            crate::agent_sessions::actor_chain("child-subagent").map(|chain| chain.to_json_value()),
            Some(serde_json::json!({
                "sub": "user:kenneth",
                "act": {
                    "sub": "research-worker",
                    "act": {
                        "sub": "agent:root"
                    }
                }
            }))
        );

        let parent_events = crate::agent_sessions::snapshot(&parent)
            .and_then(|value| value.as_dict().cloned())
            .and_then(|dict| dict.get("events").cloned())
            .and_then(|value| match value {
                VmValue::List(list) => Some((*list).clone()),
                _ => None,
            })
            .expect("parent events");
        let event_kinds: Vec<String> = parent_events
            .iter()
            .filter_map(|event| event.as_dict())
            .filter_map(|dict| dict.get("kind").map(VmValue::display))
            .collect();
        assert!(event_kinds.iter().any(|kind| kind == "sub_agent_start"));
        assert!(event_kinds.iter().any(|kind| kind == "sub_agent_result"));

        reset_llm_mock_state();
        crate::agent_sessions::reset_session_store();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_sub_agent_propagates_nested_budget_denial() {
        crate::agent_sessions::reset_session_store();
        reset_llm_mock_state();
        let _policy = push_recursion_policy(0);
        let parent = crate::agent_sessions::open_or_create(Some("parent-budget".into()));
        let mut options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (crate::value::intern_key("max_iterations"), VmValue::Int(1)),
        ]);
        annotate_nested_execution_options(
            &mut options,
            NestedExecutionKind::SubAgentRun,
            "budgeted-worker",
        );

        let spec = SubAgentRunSpec {
            name: "budgeted-worker".to_string(),
            task: "inspect the repo".to_string(),
            system: None,
            options,
            returns_schema: None,
            session_id: "child-budget".to_string(),
            parent_session_id: Some(parent.clone()),
            reminder_propagation: Vec::new(),
            workspace_anchor: None,
        };

        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
        let result = execute_sub_agent(&ctx, spec).await.unwrap();

        assert_eq!(result.payload["ok"].as_bool(), Some(false));
        assert_eq!(result.payload["budget_exceeded"].as_bool(), Some(true));
        assert_eq!(
            result.payload["error"]["category"].as_str(),
            Some("budget_exceeded")
        );
        assert!(
            result.payload["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("sub_agent_run")),
            "{:?}",
            result.payload
        );
        assert!(result
            .payload
            .get("transcript")
            .and_then(|transcript| transcript.get("events"))
            .and_then(|events| events.as_array())
            .is_some_and(|events| events
                .iter()
                .any(|event| event["kind"] == "nested_execution_budget_denied")));

        let parent_events = crate::agent_sessions::snapshot(&parent)
            .and_then(|value| value.as_dict().cloned())
            .and_then(|dict| dict.get("events").cloned())
            .and_then(|value| match value {
                VmValue::List(list) => Some((*list).clone()),
                _ => None,
            })
            .expect("parent events");
        let result_event = parent_events
            .iter()
            .filter_map(|event| event.as_dict())
            .find(|dict| {
                dict.get("kind").map(VmValue::display).as_deref() == Some("sub_agent_result")
            })
            .expect("sub_agent_result event");
        let metadata = result_event
            .get("metadata")
            .and_then(|value| value.as_dict())
            .expect("event metadata");
        assert!(matches!(metadata.get("ok"), Some(VmValue::Bool(false))));
        assert!(matches!(
            metadata.get("budget_exceeded"),
            Some(VmValue::Bool(true))
        ));

        reset_llm_mock_state();
        crate::agent_sessions::reset_session_store();
    }
}
