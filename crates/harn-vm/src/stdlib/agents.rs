//! Agent orchestration primitives.
//!
//! Provides `agent()` for creating named, configured agents, and `agent_call()`
//! for invoking them. These are ergonomic wrappers around `agent_loop` that
//! make multi-agent pipelines natural to express.

#[path = "agents_workers.rs"]
pub(super) mod agents_workers;
#[path = "records.rs"]
pub(super) mod records;
#[path = "workflow/mod.rs"]
pub(super) mod workflow;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use self::agents_workers::{
    apply_worker_artifact_policy, emit_worker_event, load_worker_state_snapshot, next_worker_id,
    parse_worker_config, persist_worker_state_snapshot, spawn_worker_task, with_worker_state,
    worker_id_from_value, worker_snapshot_path, worker_summary, WorkerConfig, WorkerState,
    WORKER_REGISTRY,
};
use crate::orchestration::{
    normalize_workflow_value, pop_execution_policy, push_execution_policy, select_artifacts,
    ArtifactRecord, CapabilityPolicy, ContextPolicy, MutationSessionRecord, WorkflowGraph,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) use self::records::{parse_artifact_list, parse_context_policy};
fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("agents encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

#[derive(Clone, Debug, Default)]
pub(super) struct SubAgentRunSpec {
    pub(super) name: String,
    pub(super) task: String,
    pub(super) system: Option<String>,
    pub(super) options: BTreeMap<String, VmValue>,
    pub(super) returns_schema: Option<VmValue>,
    pub(super) session_id: String,
    pub(super) parent_session_id: Option<String>,
}

pub(super) struct SubAgentExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: VmValue,
}

struct ParsedSubAgentRequest {
    spec: SubAgentRunSpec,
    background: bool,
    execution: agents_workers::WorkerExecutionProfile,
    worker_policy: Option<CapabilityPolicy>,
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

fn parse_sub_agent_request(args: &[VmValue]) -> Result<ParsedSubAgentRequest, VmError> {
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
            options.insert("policy".to_string(), to_vm(&policy)?);
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
    VmValue::Dict(Rc::new(envelope))
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

fn summarize_sub_agent_result(result: &serde_json::Value) -> String {
    let raw = result
        .get("visible_text")
        .and_then(|value| value.as_str())
        .or_else(|| result.get("text").and_then(|value| value.as_str()))
        .unwrap_or_default();
    let sanitized = crate::visible_text::sanitize_visible_assistant_text(raw, false);
    if sanitized.trim().is_empty() {
        raw.trim().to_string()
    } else {
        sanitized.trim().to_string()
    }
}

fn parse_structured_sub_agent_data(summary: &str, schema: &VmValue) -> Result<VmValue, VmError> {
    let json = crate::stdlib::json::extract_json_from_text(summary);
    let parsed = serde_json::from_str::<serde_json::Value>(&json).map_err(|error| {
        VmError::CategorizedError {
            message: format!("sub_agent_run: child summary was not valid JSON: {error}"),
            category: crate::value::ErrorCategory::SchemaValidation,
        }
    })?;
    crate::schema::schema_expect_value(&crate::stdlib::json_to_vm_value(&parsed), schema, false)
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
    let turn_policy = options
        .as_ref()
        .and_then(|o| o.get("turn_policy"))
        .map(|v| {
            serde_json::from_value::<crate::orchestration::TurnPolicy>(
                crate::llm::helpers::vm_value_to_json(v),
            )
            .unwrap_or_default()
        });
    let (skill_registry, skill_match, working_files) = crate::llm::parse_skill_config(&options);
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
        auto_compact: None,
        policy,
        approval_policy,
        daemon: false,
        daemon_config: Default::default(),
        llm_retries: crate::llm::helpers::opt_int(&options, "llm_retries").unwrap_or(3) as usize,
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

    let args = vec![
        VmValue::String(Rc::from(spec.task.clone())),
        spec.system
            .as_ref()
            .map(|system| VmValue::String(Rc::from(system.clone())))
            .unwrap_or(VmValue::Nil),
        VmValue::Dict(Rc::new(spec.options.clone())),
    ];
    let mut llm_opts = crate::llm::helpers::extract_llm_options(&args)?;
    let mut config = sub_agent_loop_options(&spec)?;
    if config.tool_format == "text" {
        config.tool_format =
            crate::llm_config::default_tool_format(&llm_opts.model, &llm_opts.provider);
    }
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
                error_value,
            );
            return Ok(SubAgentExecutionResult {
                payload: crate::llm::vm_value_to_json(&envelope),
                transcript,
            });
        }
    };
    let tokens_used = transcript_tokens_used(&transcript);

    let summary = summarize_sub_agent_result(&result);
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

    if let Some(schema) = spec.returns_schema.as_ref() {
        match parse_structured_sub_agent_data(&summary, schema) {
            Ok(data) => {
                envelope.insert("data".to_string(), data);
            }
            Err(error) => {
                let message = error.to_string();
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
                    )),
                    transcript,
                });
            }
        }
    }

    if let Some((tool, reason)) = permission_denied_from_transcript(&transcript) {
        return Ok(SubAgentExecutionResult {
            payload: crate::llm::vm_value_to_json(&wrap_sub_agent_error(
                summary,
                artifacts,
                evidence_added,
                tokens_used,
                budget_exceeded,
                &spec.session_id,
                sub_agent_error_dict("permission_denied", reason, Some(tool)),
            )),
            transcript,
        });
    }

    Ok(SubAgentExecutionResult {
        payload: crate::llm::vm_value_to_json(&VmValue::Dict(Rc::new(envelope))),
        transcript,
    })
}

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
    vm.register_builtin("agent", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let config = match args.get(1) {
            Some(VmValue::Dict(map)) => (**map).clone(),
            Some(_) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent: second argument must be a config dict",
                ))));
            }
            None => BTreeMap::new(),
        };

        let mut agent = config;
        agent.insert("_type".to_string(), VmValue::String(Rc::from("agent")));
        agent.insert("name".to_string(), VmValue::String(Rc::from(name)));

        Ok(VmValue::Dict(Rc::new(agent)))
    });

    vm.register_builtin("agent_config", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "agent_config: requires agent and prompt",
            ))));
        }

        let agent = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_config: first argument must be an agent",
                ))));
            }
        };

        match agent.get("_type") {
            Some(VmValue::String(t)) if &**t == "agent" => {}
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_config: first argument must be an agent (created with agent())",
                ))));
            }
        }

        let mut options = BTreeMap::new();
        for key in [
            "provider",
            "model",
            "tools",
            "max_iterations",
            "tool_format",
            "context_callback",
            "context_filter",
            "tool_retries",
            "tool_backoff_ms",
        ] {
            if let Some(val) = agent.get(key) {
                options.insert(key.to_string(), val.clone());
            }
        }

        let prompt = args[1].clone();
        let system = agent.get("system").cloned().unwrap_or(VmValue::Nil);

        let mut result = BTreeMap::new();
        result.insert("prompt".to_string(), prompt);
        result.insert("system".to_string(), system);
        result.insert("options".to_string(), VmValue::Dict(Rc::new(options)));

        Ok(VmValue::Dict(Rc::new(result)))
    });

    vm.register_builtin("agent_name", |args, _out| {
        let agent = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_name: argument must be an agent",
                ))));
            }
        };
        Ok(agent.get("name").cloned().unwrap_or(VmValue::Nil))
    });

    vm.register_async_builtin("sub_agent_run", |args| async move {
        let request = parse_sub_agent_request(&args)?;
        if !request.background {
            let result = execute_sub_agent(request.spec).await?;
            return Ok(crate::stdlib::json_to_vm_value(&result.payload));
        }

        let worker_id = next_worker_id();
        let created_at = uuid::Uuid::now_v7().to_string();
        let mut audit = agents_workers::inherited_worker_audit("sub_agent");
        audit.worker_id = Some(worker_id.clone());
        let state = Rc::new(RefCell::new(WorkerState {
            id: worker_id.clone(),
            name: request.spec.name.clone(),
            task: request.spec.task.clone(),
            status: "running".to_string(),
            created_at: created_at.clone(),
            started_at: created_at,
            finished_at: None,
            mode: "sub_agent".to_string(),
            history: vec![request.spec.task.clone()],
            config: WorkerConfig::SubAgent {
                spec: Box::new(request.spec),
            },
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
            latest_payload: None,
            latest_error: None,
            transcript: None,
            artifacts: Vec::new(),
            parent_worker_id: None,
            parent_stage_id: None,
            child_run_id: None,
            child_run_path: None,
            carry_policy: agents_workers::WorkerCarryPolicy {
                artifact_mode: "inherit".to_string(),
                context_policy: ContextPolicy::default(),
                resume_workflow: false,
                persist_state: true,
                policy: request.worker_policy,
            },
            execution: request.execution,
            snapshot_path: worker_snapshot_path(&worker_id),
            audit,
        }));
        {
            let worker = state.borrow();
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
        }
        WORKER_REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .insert(worker_id.clone(), state.clone());
        });
        spawn_worker_task(state.clone());
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("spawn_agent", |args| async move {
        let config = args
            .first()
            .ok_or_else(|| VmError::Runtime("spawn_agent: missing config".to_string()))?;
        let init = parse_worker_config(config)?;
        let worker_id = next_worker_id();
        let created_at = uuid::Uuid::now_v7().to_string();
        let mode = match &init.config {
            WorkerConfig::Workflow { .. } => "workflow",
            WorkerConfig::Stage { .. } => "stage",
            WorkerConfig::SubAgent { .. } => "sub_agent",
        }
        .to_string();
        let mut audit = init.audit.clone().normalize();
        audit.worker_id = Some(worker_id.clone());
        audit.execution_kind = Some(mode.clone());
        let state = Rc::new(RefCell::new(WorkerState {
            id: worker_id.clone(),
            name: init.name,
            task: init.task.clone(),
            status: "running".to_string(),
            created_at: created_at.clone(),
            started_at: created_at,
            finished_at: None,
            mode,
            history: vec![init.task],
            config: init.config,
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
            latest_payload: None,
            latest_error: None,
            transcript: None,
            artifacts: Vec::new(),
            parent_worker_id: None,
            parent_stage_id: None,
            child_run_id: None,
            child_run_path: None,
            carry_policy: init.carry_policy,
            execution: init.execution,
            snapshot_path: worker_snapshot_path(&worker_id),
            audit,
        }));
        {
            let worker = state.borrow();
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
        }
        WORKER_REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .insert(worker_id.clone(), state.clone());
        });
        spawn_worker_task(state.clone());
        if init.wait {
            let handle =
                state.borrow_mut().handle.take().ok_or_else(|| {
                    VmError::Runtime("spawn_agent: worker did not start".to_string())
                })?;
            let _ = handle.await.map_err(|error| {
                VmError::Runtime(format!("spawn_agent worker join error: {error}"))
            })??;
        }
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("send_input", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Runtime(
                "send_input: requires worker handle and task text".to_string(),
            ));
        }
        let worker_id = worker_id_from_value(&args[0])?;
        let next_task = args[1].display();
        if next_task.is_empty() {
            return Err(VmError::Runtime(
                "send_input: task text must not be empty".to_string(),
            ));
        }
        with_worker_state(&worker_id, |state| {
            let mut worker = state.borrow_mut();
            if worker.status == "running" {
                return Err(VmError::Runtime(format!(
                    "send_input: worker {} is still running",
                    worker.id
                )));
            }
            worker.cancel_token = Arc::new(AtomicBool::new(false));
            worker.task = next_task.clone();
            worker.history.push(next_task.clone());
            worker.status = "running".to_string();
            worker.started_at = uuid::Uuid::now_v7().to_string();
            worker.finished_at = None;
            worker.latest_error = None;
            worker.latest_payload = None;
            let next_artifacts =
                apply_worker_artifact_policy(&worker.artifacts, &worker.carry_policy);
            // Session continuity is expressed explicitly via `agent_session_fork`
            // / `agent_session_reset` at the call site, so the worker's next
            // transcript is simply whatever the session store holds for its id.
            let next_transcript = worker.transcript.clone();
            let worker_parent = worker.id.clone();
            let resume_workflow = worker.carry_policy.resume_workflow;
            let child_run_path = worker.child_run_path.clone();
            match &mut worker.config {
                WorkerConfig::Workflow {
                    artifacts, options, ..
                } => {
                    if !next_artifacts.is_empty() {
                        *artifacts = next_artifacts.clone();
                    }
                    options.insert(
                        "parent_worker_id".to_string(),
                        VmValue::String(Rc::from(worker_parent)),
                    );
                    if resume_workflow {
                        if let Some(child_run_path) = child_run_path {
                            options.insert(
                                "resume_path".to_string(),
                                VmValue::String(Rc::from(child_run_path)),
                            );
                        }
                    } else {
                        options.remove("resume_path");
                    }
                }
                WorkerConfig::Stage {
                    artifacts,
                    transcript,
                    ..
                } => {
                    if !next_artifacts.is_empty() {
                        *artifacts = next_artifacts.clone();
                    }
                    *transcript = next_transcript;
                }
                WorkerConfig::SubAgent { spec } => {
                    spec.task = next_task.clone();
                }
            }
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            drop(worker);
            spawn_worker_task(state.clone());
            let summary = worker_summary(&state.borrow())?;
            Ok(summary)
        })
    });

    vm.register_builtin("resume_agent", |args, _out| {
        let target = args
            .first()
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                VmError::Runtime("resume_agent: missing worker id or snapshot path".to_string())
            })?;
        let state = Rc::new(RefCell::new(load_worker_state_snapshot(&target)?));
        let worker_id = state.borrow().id.clone();
        WORKER_REGISTRY.with(|registry| {
            registry.borrow_mut().insert(worker_id, state.clone());
        });
        if state.borrow().carry_policy.persist_state {
            persist_worker_state_snapshot(&state.borrow())?;
        }
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("wait_agent", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("wait_agent: missing worker handle".to_string()))?;
        if let VmValue::List(list) = target {
            let mut results = Vec::new();
            for item in list.iter() {
                let worker_id = worker_id_from_value(item)?;
                let state = with_worker_state(&worker_id, Ok)?;
                let handle = state.borrow_mut().handle.take();
                if let Some(handle) = handle {
                    let _ = handle.await.map_err(|error| {
                        VmError::Runtime(format!("wait_agent join error: {error}"))
                    })??;
                }
                results.push(worker_summary(&state.borrow())?);
            }
            return Ok(VmValue::List(Rc::new(results)));
        }
        let worker_id = worker_id_from_value(target)?;
        let state = with_worker_state(&worker_id, Ok)?;
        let handle = state.borrow_mut().handle.take();
        if let Some(handle) = handle {
            let _ = handle
                .await
                .map_err(|error| VmError::Runtime(format!("wait_agent join error: {error}")))??;
        }
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_builtin("close_agent", |args, _out| {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("close_agent: missing worker handle".to_string()))?;
        let worker_id = worker_id_from_value(target)?;
        with_worker_state(&worker_id, |state| {
            let mut worker = state.borrow_mut();
            worker.cancel_token.store(true, Ordering::SeqCst);
            if let Some(handle) = worker.handle.take() {
                handle.abort();
            }
            worker.status = "cancelled".to_string();
            worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
            worker.latest_error = Some("worker cancelled".to_string());
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            emit_worker_event(&worker, "cancelled");
            let summary = worker_summary(&worker)?;
            Ok(summary)
        })
    });

    vm.register_builtin("list_agents", |_args, _out| {
        let workers = WORKER_REGISTRY.with(|registry| {
            registry
                .borrow()
                .values()
                .map(|state| worker_summary(&state.borrow()))
                .collect::<Result<Vec<_>, _>>()
        })?;
        Ok(VmValue::List(Rc::new(workers)))
    });

    records::register_record_builtins(vm);
    workflow::register_workflow_builtins(vm);
}
