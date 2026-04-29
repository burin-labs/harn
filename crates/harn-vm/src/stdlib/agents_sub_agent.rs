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

fn select_tool_registry(registry: &VmValue, names: &[String]) -> Result<VmValue, VmError> {
    let registry_dict = registry.as_dict().ok_or_else(|| {
        VmError::Runtime("sub_agent_run: tools must be a tool registry".to_string())
    })?;
    let is_registry = matches!(
        registry_dict.get("_type"),
        Some(VmValue::String(kind)) if kind.as_ref() == "tool_registry"
    );
    if !is_registry {
        return Err(VmError::Runtime(
            "sub_agent_run: tools must be a tool registry".to_string(),
        ));
    }
    let selected: Vec<VmValue> = registry_dict
        .get("tools")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
        .map(|list| {
            list.iter()
                .filter(|tool| {
                    tool.as_dict()
                        .and_then(|entry| entry.get("name"))
                        .map(|value| names.iter().any(|name| name == &value.display()))
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut next = registry_dict.clone();
    next.insert("tools".to_string(), VmValue::List(Rc::new(selected)));
    Ok(VmValue::Dict(Rc::new(next)))
}

fn sub_agent_requested_policy(
    options: &BTreeMap<String, VmValue>,
    allowed_tools: &[String],
) -> Result<Option<CapabilityPolicy>, VmError> {
    let explicit: Option<CapabilityPolicy> = options
        .get("policy")
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

pub(super) fn parse_sub_agent_request(args: &[VmValue]) -> Result<ParsedSubAgentRequest, VmError> {
    let task = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| VmError::Runtime("sub_agent_run: task is required".to_string()))?;
    let raw_options = match args.get(1) {
        Some(VmValue::Dict(map)) => map.as_ref().clone(),
        Some(VmValue::Nil) | None => BTreeMap::new(),
        Some(_) => {
            return Err(VmError::Runtime(
                "sub_agent_run: options must be a dict".to_string(),
            ))
        }
    };
    let background = matches!(raw_options.get("background"), Some(VmValue::Bool(true)));
    let allowed_tools = parse_string_list(
        raw_options.get("allowed_tools"),
        "sub_agent_run.allowed_tools",
    )?;
    let base_tools = raw_options
        .get("tools")
        .cloned()
        .or_else(crate::stdlib::tools::current_tool_registry);
    let selected_tools = if allowed_tools.is_empty() {
        base_tools
    } else {
        base_tools
            .as_ref()
            .map(|registry| select_tool_registry(registry, &allowed_tools))
            .transpose()?
    };
    let requested_policy = sub_agent_requested_policy(&raw_options, &allowed_tools)?;
    let worker_policy = agents_workers::resolve_inherited_worker_policy(requested_policy.clone())?;
    let carry_policy = agents_workers::parse_worker_carry_policy(&raw_options)?;
    let execution = agents_workers::parse_worker_execution_profile(raw_options.get("execution"))?;
    let returns_schema = raw_options
        .get("returns")
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("schema"))
        .cloned();
    let system = raw_options.get("system").and_then(|value| match value {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => None,
    });
    let session_id = raw_options
        .get("session_id")
        .and_then(|value| match value {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| format!("sub_agent_session_{}", uuid::Uuid::now_v7()));

    let mut options = raw_options.clone();
    for key in [
        "background",
        "carry",
        "returns",
        "allowed_tools",
        "name",
        "execution",
        "system",
        "session_id",
    ] {
        options.remove(key);
    }
    options.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id.clone())),
    );
    match selected_tools {
        Some(registry) => {
            options.insert("tools".to_string(), registry);
        }
        None => {
            options.remove("tools");
        }
    }
    match requested_policy {
        Some(policy) => {
            options.insert("policy".to_string(), super::to_vm(&policy)?);
        }
        None => {
            options.remove("policy");
        }
    }

    Ok(ParsedSubAgentRequest {
        spec: SubAgentRunSpec {
            name: raw_options
                .get("name")
                .map(|value| value.display())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "sub-agent".to_string()),
            task,
            system,
            options,
            returns_schema,
            session_id,
            parent_session_id: crate::llm::current_agent_session_id(),
        },
        background,
        carry_policy,
        execution,
        worker_policy,
    })
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
        let dict = event.as_dict()?;
        let rejected = dict
            .get("metadata")
            .and_then(|value| value.as_dict())
            .and_then(|metadata| metadata.get("rejected"))
            .is_some_and(|value| value.is_truthy());
        if !rejected {
            continue;
        }
        let text = dict
            .get("text")
            .map(|value| value.display())
            .unwrap_or_default();
        let json = crate::stdlib::json::extract_json_from_text(&text);
        let payload = serde_json::from_str::<serde_json::Value>(&json).ok()?;
        if payload.get("error").and_then(|value| value.as_str()) == Some("permission_denied") {
            let tool = payload
                .get("tool")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let reason = payload
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("permission denied")
                .to_string();
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
    ) || options.contains_key("json_schema")
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

fn sub_agent_loop_options(spec: &SubAgentRunSpec) -> Result<crate::llm::AgentLoopConfig, VmError> {
    let options = Some(spec.options.clone());
    let max_iterations = crate::llm::helpers::opt_int(&options, "max_iterations").unwrap_or(50);
    let max_nudges = crate::llm::helpers::opt_int(&options, "max_nudges").unwrap_or(3);
    let tool_retries = crate::llm::helpers::opt_int(&options, "tool_retries").unwrap_or(0);
    let tool_backoff_ms = crate::llm::helpers::opt_int(&options, "tool_backoff_ms").unwrap_or(1000);
    let tool_format = crate::llm::helpers::opt_str(&options, "tool_format");
    let done_sentinel = crate::llm::helpers::opt_str(&options, "done_sentinel");
    let break_unless_phase = crate::llm::helpers::opt_str(&options, "break_unless_phase");
    let policy = options.as_ref().and_then(|o| o.get("policy")).map(|v| {
        serde_json::from_value::<CapabilityPolicy>(crate::llm::helpers::vm_value_to_json(v))
            .unwrap_or_default()
    });
    let approval_policy = options
        .as_ref()
        .and_then(|o| o.get("approval_policy"))
        .map(|v| {
            serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(
                crate::llm::helpers::vm_value_to_json(v),
            )
            .unwrap_or_default()
        });
    let permissions = crate::llm::permissions::parse_dynamic_permission_policy(
        options.as_ref().and_then(|o| o.get("permissions")),
        "sub_agent_run",
    )?;
    let turn_policy = options
        .as_ref()
        .and_then(|o| o.get("turn_policy"))
        .map(|v| {
            serde_json::from_value::<crate::orchestration::TurnPolicy>(
                crate::llm::helpers::vm_value_to_json(v),
            )
            .unwrap_or_default()
        });
    let native_tool_fallback = crate::llm::helpers::opt_str(&options, "native_tool_fallback")
        .map(|value| {
            crate::orchestration::NativeToolFallbackPolicy::parse(&value).ok_or_else(|| {
                VmError::Runtime(format!(
                    "sub_agent_run: native_tool_fallback must be one of allow, allow_once, reject; got `{value}`"
                ))
            })
        })
        .transpose()?
        .unwrap_or_default();
    let (skill_registry, skill_match, working_files) = crate::llm::parse_skill_config(&options);
    let mcp_servers = crate::llm::parse_mcp_server_specs(&options)?;
    Ok(crate::llm::AgentLoopConfig {
        persistent: crate::llm::helpers::opt_bool(&options, "persistent"),
        max_iterations: max_iterations as usize,
        max_nudges: max_nudges as usize,
        nudge: crate::llm::helpers::opt_str(&options, "nudge"),
        done_sentinel,
        break_unless_phase,
        tool_retries: tool_retries as usize,
        tool_backoff_ms: tool_backoff_ms as u64,
        tool_format: tool_format.unwrap_or_default(),
        native_tool_fallback,
        auto_compact: None,
        policy,
        command_policy: crate::llm::parse_command_policy_from_options(&options, "sub_agent_run")?,
        permissions,
        approval_policy,
        daemon: false,
        daemon_config: Default::default(),
        llm_retries: crate::llm::helpers::opt_int(&options, "llm_retries")
            .unwrap_or(crate::llm::DEFAULT_AGENT_LOOP_LLM_RETRIES as i64)
            as usize,
        llm_backoff_ms: crate::llm::helpers::opt_int(&options, "llm_backoff_ms").unwrap_or(2000)
            as u64,
        token_budget: crate::llm::helpers::opt_int(&options, "token_budget"),
        exit_when_verified: crate::llm::helpers::opt_bool(&options, "exit_when_verified"),
        loop_detect_warn: crate::llm::helpers::opt_int(&options, "loop_detect_warn").unwrap_or(2)
            as usize,
        loop_detect_block: crate::llm::helpers::opt_int(&options, "loop_detect_block").unwrap_or(3)
            as usize,
        loop_detect_skip: crate::llm::helpers::opt_int(&options, "loop_detect_skip").unwrap_or(4)
            as usize,
        tool_examples: crate::llm::helpers::opt_str(&options, "tool_examples"),
        turn_policy,
        stop_after_successful_tools: crate::llm::helpers::opt_str_list(
            &options,
            "stop_after_successful_tools",
        ),
        require_successful_tools: crate::llm::helpers::opt_str_list(
            &options,
            "require_successful_tools",
        ),
        session_id: spec.session_id.clone(),
        event_sink: None,
        task_ledger: Default::default(),
        post_turn_callback: options
            .as_ref()
            .and_then(|o| o.get("post_turn_callback"))
            .filter(|v| matches!(v, VmValue::Closure(_)))
            .cloned(),
        skill_registry,
        skill_match,
        working_files,
        mcp_servers,
        mcp_clients: Default::default(),
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

    let args = vec![
        VmValue::String(Rc::from(spec.task.clone())),
        spec.system
            .as_ref()
            .map(|system| VmValue::String(Rc::from(system.clone())))
            .unwrap_or(VmValue::Nil),
        VmValue::Dict(Rc::new(spec.options.clone())),
    ];
    let mut llm_opts = crate::llm::helpers::extract_llm_options(&args)?;
    let config = sub_agent_loop_options(&spec)?;
    let result = crate::llm::run_agent_loop_internal(&mut llm_opts, config).await;

    let (result, transcript) = match result {
        Ok(result) => {
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
    async fn execute_sub_agent_forks_parent_context_and_appends_parent_events() {
        crate::agent_sessions::reset_session_store();
        reset_llm_mock_state();
        let parent = crate::agent_sessions::open_or_create(Some("parent-subagent".into()));
        crate::agent_sessions::inject_message(&parent, assistant_message("parent context"))
            .unwrap();
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
            stop_reason: None,
            model: "mock".to_string(),
            provider: None,
            blocks: None,
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

        let result = execute_sub_agent(spec).await.unwrap();
        assert_eq!(result.payload["ok"].as_bool(), Some(true));

        let child_messages = crate::agent_sessions::messages_json("child-subagent");
        assert_eq!(
            child_messages[0]["content"].as_str(),
            Some("parent context")
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
