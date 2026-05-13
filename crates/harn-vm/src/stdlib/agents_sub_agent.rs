use std::collections::BTreeMap;
use std::rc::Rc;

use super::agents_workers;
use super::{SubAgentExecutionResult, SubAgentRunSpec};
use crate::orchestration::CapabilityPolicy;
use crate::value::{VmError, VmValue};

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
        .map_err(|e| VmError::Runtime(format!("sub_agent_run: policy parse error: {e}")))?;
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

fn request_string_field(
    request: &BTreeMap<String, VmValue>,
    key: &str,
    fallback: Option<&str>,
) -> Option<String> {
    request.get(key).and_then(|value| match value {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => fallback.map(str::to_string),
    })
}

fn request_options(
    request: &BTreeMap<String, VmValue>,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    match request.get("options") {
        Some(VmValue::Dict(options)) => Ok(options.as_ref().clone()),
        Some(VmValue::Nil) | None => Ok(BTreeMap::new()),
        Some(_) => Err(VmError::Runtime(
            "sub_agent_run: request.options must be a dict".to_string(),
        )),
    }
}

pub(super) fn parse_sub_agent_request(args: &[VmValue]) -> Result<ParsedSubAgentRequest, VmError> {
    let request = validate_sub_agent_request_envelope(args)?;
    let task = request_string_field(&request, "task", None)
        .ok_or_else(|| VmError::Runtime("sub_agent_run: task is required".to_string()))?;
    let policies = resolve_sub_agent_policies(&request)?;
    let session_id = request_string_field(&request, "session_id", None)
        .unwrap_or_else(|| format!("sub_agent_session_{}", uuid::Uuid::now_v7()));
    let options =
        prepare_sub_agent_options(&request, &session_id, policies.requested_policy.as_ref())?;

    Ok(ParsedSubAgentRequest {
        spec: SubAgentRunSpec {
            name: request_string_field(&request, "name", Some("sub-agent"))
                .unwrap_or_else(|| "sub-agent".to_string()),
            task,
            system: request_string_field(&request, "system", None),
            options,
            returns_schema: sub_agent_returns_schema(&request),
            session_id,
            parent_session_id: crate::llm::current_agent_session_id(),
        },
        background: matches!(request.get("background"), Some(VmValue::Bool(true))),
        carry_policy: policies.carry_policy,
        execution: policies.execution,
        worker_policy: policies.worker_policy,
    })
}

fn validate_sub_agent_request_envelope(
    args: &[VmValue],
) -> Result<BTreeMap<String, VmValue>, VmError> {
    let request = match args.first() {
        Some(VmValue::Dict(map)) => map.as_ref().clone(),
        _ => return Err(invalid_sub_agent_request()),
    };
    if matches!(
        request.get("_type"),
        Some(VmValue::String(kind)) if kind.as_ref() == "sub_agent_request"
    ) {
        return Ok(request);
    }
    Err(invalid_sub_agent_request())
}

fn invalid_sub_agent_request() -> VmError {
    VmError::Runtime("sub_agent_run: expected a normalized sub_agent_request dict".to_string())
}

fn resolve_sub_agent_policies(
    request: &BTreeMap<String, VmValue>,
) -> Result<SubAgentPolicyResolution, VmError> {
    let allowed_tools =
        parse_string_list(request.get("allowed_tools"), "sub_agent_run.allowed_tools")?;
    let requested_policy = sub_agent_requested_policy(request.get("policy"), &allowed_tools)?;
    let worker_policy = agents_workers::resolve_inherited_worker_policy(requested_policy.clone())?;
    let carry_policy = agents_workers::parse_worker_carry_policy(request)?;
    let execution = agents_workers::parse_worker_execution_profile(request.get("execution"))?;
    Ok(SubAgentPolicyResolution {
        requested_policy,
        worker_policy,
        carry_policy,
        execution,
    })
}

fn sub_agent_returns_schema(request: &BTreeMap<String, VmValue>) -> Option<VmValue> {
    request
        .get("returns_schema")
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned()
        .or_else(|| {
            request
                .get("returns")
                .and_then(|value| value.as_dict())
                .and_then(|dict| dict.get("schema"))
                .cloned()
        })
}

fn prepare_sub_agent_options(
    request: &BTreeMap<String, VmValue>,
    session_id: &str,
    requested_policy: Option<&CapabilityPolicy>,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    let mut options = request_options(request)?;
    inject_sub_agent_skill_context(&mut options);
    options.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id.to_string())),
    );
    match requested_policy {
        Some(policy) => {
            options.insert("policy".to_string(), super::to_vm(policy)?);
        }
        None => {
            options.remove("policy");
        }
    }
    Ok(options)
}

fn inject_sub_agent_skill_context(options: &mut BTreeMap<String, VmValue>) {
    let Some(context) = crate::orchestration::current_workflow_skill_context() else {
        return;
    };
    if !options.contains_key("skills") {
        if let Some(registry) = context.registry {
            options.insert("skills".to_string(), registry);
        }
    }
    if !options.contains_key("skill_match") {
        if let Some(match_config) = context.match_config {
            options.insert("skill_match".to_string(), match_config);
        }
    }
}

fn sub_agent_error_dict(
    category: &str,
    message: impl Into<String>,
    tool: Option<String>,
) -> VmValue {
    let mut error = BTreeMap::new();
    error.insert(
        "category".to_string(),
        VmValue::String(Rc::from(category.to_string())),
    );
    error.insert(
        "message".to_string(),
        VmValue::String(Rc::from(message.into())),
    );
    if let Some(tool) = tool {
        error.insert("tool".to_string(), VmValue::String(Rc::from(tool)));
    }
    VmValue::Dict(Rc::new(error))
}

fn sub_agent_base_envelope(
    summary: String,
    artifacts: VmValue,
    evidence_added: i64,
    tokens_used: i64,
    budget_exceeded: bool,
    session_id: &str,
) -> BTreeMap<String, VmValue> {
    let mut envelope = BTreeMap::new();
    envelope.insert("ok".to_string(), VmValue::Bool(true));
    envelope.insert("summary".to_string(), VmValue::String(Rc::from(summary)));
    envelope.insert("artifacts".to_string(), artifacts);
    envelope.insert("evidence_added".to_string(), VmValue::Int(evidence_added));
    envelope.insert("tokens_used".to_string(), VmValue::Int(tokens_used));
    envelope.insert(
        "budget_exceeded".to_string(),
        VmValue::Bool(budget_exceeded),
    );
    envelope.insert("data".to_string(), VmValue::Nil);
    envelope.insert("error".to_string(), VmValue::Nil);
    envelope.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id.to_string())),
    );
    envelope
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
    envelope.insert("ok".to_string(), VmValue::Bool(false));
    envelope.insert("error".to_string(), error);
    if let Some(transcript) = transcript {
        envelope.insert("transcript".to_string(), transcript);
    }
    VmValue::Dict(Rc::new(envelope))
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

fn option_requests_structured_output(options: &BTreeMap<String, VmValue>) -> bool {
    matches!(
        options.get("response_format"),
        Some(VmValue::String(value)) if value.as_ref() == "json"
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
    spec: SubAgentRunSpec,
) -> Result<SubAgentExecutionResult, VmError> {
    if let Some(parent_session_id) = spec.parent_session_id.as_deref() {
        crate::agent_sessions::open_child_session(parent_session_id, Some(spec.session_id.clone()));
    } else {
        crate::agent_sessions::open_or_create(Some(spec.session_id.clone()));
    }
    append_parent_sub_agent_event(
        spec.parent_session_id.as_deref(),
        sub_agent_start_event(&spec),
    );

    let mut loop_options = spec.options.clone();
    loop_options.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(spec.session_id.clone())),
    );
    let args = vec![
        VmValue::String(Rc::from(spec.task.clone())),
        spec.system
            .as_ref()
            .map(|system| VmValue::String(Rc::from(system.clone())))
            .unwrap_or(VmValue::Nil),
        VmValue::Dict(Rc::new(loop_options)),
    ];
    let result = crate::stdlib::harn_entry::call_harn_export_by_name(
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
            let transcript = crate::agent_sessions::snapshot(&spec.session_id)
                .unwrap_or_else(|| crate::stdlib::json_to_vm_value(&serde_json::json!({})));
            let tokens_used = transcript_tokens_used(&transcript);
            let envelope = wrap_sub_agent_error(
                String::new(),
                VmValue::List(Rc::new(Vec::new())),
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

    let wants_structured_output =
        spec.returns_schema.is_some() || option_requests_structured_output(&spec.options);
    let synthesized = synthesize_sub_agent_result(&result, &transcript, wants_structured_output);
    let summary = synthesized.summary.clone();
    let artifacts = transcript
        .as_dict()
        .and_then(|dict| dict.get("assets"))
        .cloned()
        .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new())));
    let evidence_added = match &artifacts {
        VmValue::List(list) => list.len() as i64,
        _ => 0,
    };
    let budget_limit = spec
        .options
        .get("token_budget")
        .and_then(|value| value.as_int())
        .unwrap_or(-1);
    let budget_exceeded = budget_limit >= 0 && tokens_used >= budget_limit;

    let mut envelope = sub_agent_base_envelope(
        summary.clone(),
        artifacts.clone(),
        evidence_added,
        tokens_used,
        budget_exceeded,
        &spec.session_id,
    );
    envelope.insert("transcript".to_string(), transcript.clone());

    if spec.returns_schema.is_none() && option_requests_structured_output(&spec.options) {
        if let Some(candidate) = synthesized.structured_json.as_ref() {
            envelope.insert(
                "data".to_string(),
                crate::stdlib::json_to_vm_value(&candidate.value),
            );
        }
    }

    if let Some(schema) = spec.returns_schema.as_ref() {
        match parse_structured_sub_agent_data(synthesized.structured_json.as_ref(), schema) {
            Ok(data) => {
                envelope.insert("data".to_string(), data);
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
        payload: crate::llm::vm_value_to_json(&VmValue::Dict(Rc::new(envelope))),
        transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::{push_llm_mock, reset_llm_mock_state, LlmMock};

    fn assistant_message(text: &str) -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("assistant"))),
            ("content".to_string(), VmValue::String(Rc::from(text))),
        ])))
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
    #[ignore = "child session tool_format claim not yet wired through Harn-driven loop — tracked in #1236"]
    async fn execute_sub_agent_uses_child_transcript_and_appends_parent_events() {
        crate::agent_sessions::reset_session_store();
        reset_llm_mock_state();
        let parent = crate::agent_sessions::open_or_create(Some("parent-subagent".into()));
        crate::agent_sessions::inject_message(&parent, assistant_message("parent context"))
            .unwrap();
        crate::agent_sessions::claim_tool_format(&parent, "text").unwrap();
        push_llm_mock(LlmMock {
            text: "child result".to_string(),
            tool_calls: Vec::new(),
            match_pattern: None,
            consume_on_match: true,
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
        });

        let spec = SubAgentRunSpec {
            name: "research-worker".to_string(),
            task: "inspect the repo".to_string(),
            system: None,
            options: BTreeMap::from([
                ("provider".to_string(), VmValue::String(Rc::from("mock"))),
                ("model".to_string(), VmValue::String(Rc::from("mock"))),
                ("max_iterations".to_string(), VmValue::Int(1)),
            ]),
            returns_schema: None,
            session_id: "child-subagent".to_string(),
            parent_session_id: Some(parent.clone()),
        };

        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let _vm_context = crate::vm::install_async_builtin_child_vm(vm);
        let result = execute_sub_agent(spec).await.unwrap();
        assert_eq!(result.payload["ok"].as_bool(), Some(true));

        let child_messages = crate::agent_sessions::messages_json("child-subagent");
        assert!(!child_messages
            .iter()
            .any(|message| message["content"].as_str() == Some("parent context")));
        assert_eq!(
            crate::agent_sessions::tool_format("child-subagent").as_deref(),
            Some("text")
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
}
