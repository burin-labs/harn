use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::helpers::{append_message_to_contexts, runtime_feedback_message};
use super::state::AgentLoopState;
use super::{emit_agent_event, AgentEvent};

const MAX_JUDGE_TRANSCRIPT_CHARS: usize = 32_000;
const MAX_JUDGE_MESSAGE_CHARS: usize = 4_000;

#[derive(Clone, Debug)]
pub(crate) struct CompletionJudgeConfig {
    pub system: String,
    pub feedback_fallback: String,
    pub max_feedback_chars: i64,
    pub options: BTreeMap<String, VmValue>,
}

impl Default for CompletionJudgeConfig {
    fn default() -> Self {
        Self {
            system: crate::stdlib::template::render_stdlib_prompt_asset(
                "agent/prompts/completion_judge_default.harn.prompt",
                None,
            )
            .unwrap_or_else(|error| {
                format!("completion judge system prompt render error: {error}")
            }),
            feedback_fallback: crate::stdlib::template::render_stdlib_prompt_asset(
                "agent/prompts/completion_judge_feedback_fallback.harn.prompt",
                None,
            )
            .unwrap_or_else(|error| {
                format!("completion judge feedback prompt render error: {error}")
            }),
            max_feedback_chars: 600,
            options: BTreeMap::new(),
        }
    }
}

pub(crate) fn parse_completion_judge_option(
    options: &Option<BTreeMap<String, VmValue>>,
) -> Result<Option<CompletionJudgeConfig>, VmError> {
    let Some(value) = options
        .as_ref()
        .and_then(|dict| dict.get("verify_completion_judge"))
    else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Bool(false) => Ok(None),
        VmValue::Bool(true) => Ok(Some(CompletionJudgeConfig::default())),
        VmValue::Dict(dict) => parse_completion_judge_dict(dict, "verify_completion_judge"),
        other => Err(VmError::Runtime(format!(
            "verify_completion_judge must be true, false, nil, or a dict; got {}",
            other.display()
        ))),
    }
}

pub(crate) fn parse_done_judge_option(
    options: &Option<BTreeMap<String, VmValue>>,
) -> Result<Option<CompletionJudgeConfig>, VmError> {
    let Some(value) = options.as_ref().and_then(|dict| dict.get("done_judge")) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Bool(false) => Ok(None),
        VmValue::Bool(true) => Ok(Some(CompletionJudgeConfig::default())),
        VmValue::Dict(dict) => parse_completion_judge_dict(dict, "done_judge"),
        other => Err(VmError::Runtime(format!(
            "done_judge must be true, false, nil, or a dict; got {}",
            other.display()
        ))),
    }
}

fn parse_completion_judge_dict(
    dict: &BTreeMap<String, VmValue>,
    label: &str,
) -> Result<Option<CompletionJudgeConfig>, VmError> {
    if matches!(dict.get("enabled"), Some(VmValue::Bool(false))) {
        return Ok(None);
    }
    let mut config = CompletionJudgeConfig::default();
    if let Some(value) = dict.get("system") {
        config.system = expect_string(value, &format!("{label}.system"))?;
    }
    if let Some(value) = dict.get("feedback_fallback") {
        config.feedback_fallback = expect_string(value, &format!("{label}.feedback_fallback"))?;
    }
    if let Some(value) = dict.get("max_feedback_chars") {
        let max_feedback_chars = value.as_int().ok_or_else(|| {
            VmError::Runtime(format!("{label}.max_feedback_chars must be an integer"))
        })?;
        if max_feedback_chars < 1 {
            return Err(VmError::Runtime(format!(
                "{label}.max_feedback_chars must be positive"
            )));
        }
        config.max_feedback_chars = max_feedback_chars;
    }
    if let Some(value) = dict.get("options") {
        let nested = value
            .as_dict()
            .ok_or_else(|| VmError::Runtime(format!("{label}.options must be a dict")))?;
        config.options.extend(
            nested
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    for (key, value) in dict {
        if matches!(
            key.as_str(),
            "enabled" | "system" | "feedback_fallback" | "max_feedback_chars" | "options"
        ) {
            continue;
        }
        config.options.insert(key.clone(), value.clone());
    }
    Ok(Some(config))
}

fn expect_string(value: &VmValue, field: &str) -> Result<String, VmError> {
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        _ => Err(VmError::Runtime(format!("{field} must be a string"))),
    }
}

pub(super) async fn run_completion_judge(
    state: &mut AgentLoopState,
    config: &CompletionJudgeConfig,
    main_opts: &crate::llm::api::LlmCallOptions,
    session_id: &str,
    iteration: usize,
    stop_reason: &str,
    last_text: &str,
    feedback_kind: &str,
) -> Result<bool, VmError> {
    let prompt = build_judge_prompt(state, session_id, iteration, stop_reason, last_text);
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "anyOf": [
            {"required": ["pass"]},
            {"required": ["verdict"]}
        ],
        "properties": {
            "pass": {"type": "boolean"},
            "verdict": {"type": "string", "enum": ["done", "continue"]},
            "reasoning": {"type": ["string", "null"], "maxLength": 2000},
            "feedback": {"type": ["string", "null"], "maxLength": config.max_feedback_chars},
            "next_step": {"type": ["string", "null"], "maxLength": config.max_feedback_chars},
            "final_response": {"type": ["string", "null"], "maxLength": 20000},
        },
    });
    let options = build_judge_options(config, main_opts);
    let judge_started = std::time::Instant::now();
    let result = crate::llm::structured_envelope::llm_call_structured_result_impl(
        vec![
            VmValue::String(Rc::from(prompt)),
            crate::stdlib::json_to_vm_value(&schema),
            VmValue::Dict(Rc::new(options)),
        ],
        state.bridge.as_ref(),
    )
    .await?;
    let judge_duration_ms = judge_started.elapsed().as_millis() as u64;
    let Some(result_dict) = result.as_dict() else {
        crate::events::log_warn(
            "agent.completion_judge",
            "structured judge returned a non-dict envelope; vetoing completion with fallback feedback",
        );
        emit_judge_decision(
            session_id,
            iteration,
            "continue",
            "structured judge returned a non-dict envelope",
            Some(config.feedback_fallback.clone()),
            judge_duration_ms,
        )
        .await;
        inject_judge_feedback(state, session_id, feedback_kind, &config.feedback_fallback).await;
        return Ok(true);
    };
    if !result_dict
        .get("ok")
        .is_some_and(|value| matches!(value, VmValue::Bool(true)))
    {
        crate::events::log_warn(
            "agent.completion_judge",
            "structured judge failed; vetoing completion with fallback feedback",
        );
        emit_judge_decision(
            session_id,
            iteration,
            "continue",
            "structured judge call failed",
            Some(config.feedback_fallback.clone()),
            judge_duration_ms,
        )
        .await;
        inject_judge_feedback(state, session_id, feedback_kind, &config.feedback_fallback).await;
        return Ok(true);
    }
    let data = result_dict
        .get("data")
        .and_then(VmValue::as_dict)
        .cloned()
        .unwrap_or_default();
    let outcome = interpret_judge_data(&data, &config.feedback_fallback);
    emit_judge_decision(
        session_id,
        iteration,
        if outcome.pass { "done" } else { "continue" },
        &outcome.reasoning,
        outcome.next_step.clone(),
        judge_duration_ms,
    )
    .await;
    if outcome.pass {
        if let Some(final_response) = data
            .get("final_response")
            .and_then(|value| match value {
                VmValue::String(text) => Some(text.trim().to_string()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
        {
            state.last_iteration_text = final_response;
        }
        return Ok(false);
    }
    let feedback = outcome
        .feedback
        .or(outcome.next_step)
        .unwrap_or_else(|| config.feedback_fallback.clone());
    inject_judge_feedback(state, session_id, feedback_kind, &feedback).await;
    Ok(true)
}

struct JudgeOutcome {
    pass: bool,
    reasoning: String,
    feedback: Option<String>,
    next_step: Option<String>,
}

fn interpret_judge_data(data: &BTreeMap<String, VmValue>, feedback_fallback: &str) -> JudgeOutcome {
    let verdict = data
        .get("verdict")
        .and_then(vm_string)
        .map(|value| value.trim().to_ascii_lowercase());
    let pass = match verdict.as_deref() {
        Some("done") => true,
        Some("continue") => false,
        _ => data
            .get("pass")
            .is_some_and(|value| matches!(value, VmValue::Bool(true))),
    };
    let reasoning = data
        .get("reasoning")
        .and_then(vm_string)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| {
            if pass {
                "judge confirmed completion".to_string()
            } else {
                "judge requested another turn".to_string()
            }
        });
    let next_step = data
        .get("next_step")
        .and_then(vm_string)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    let feedback = data
        .get("feedback")
        .and_then(vm_string)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| (!pass && next_step.is_none()).then(|| feedback_fallback.to_string()));
    JudgeOutcome {
        pass,
        reasoning,
        feedback,
        next_step,
    }
}

fn vm_string(value: &VmValue) -> Option<String> {
    match value {
        VmValue::String(text) => Some(text.to_string()),
        _ => None,
    }
}

fn build_judge_prompt(
    state: &AgentLoopState,
    session_id: &str,
    iteration: usize,
    stop_reason: &str,
    last_text: &str,
) -> String {
    let transcript = render_judge_transcript(state);
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "stop_reason".to_string(),
        VmValue::String(Rc::from(stop_reason)),
    );
    bindings.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id)),
    );
    bindings.insert("iteration".to_string(), VmValue::Int(iteration as i64));
    bindings.insert(
        "all_tools_used".to_string(),
        VmValue::String(Rc::from(state.all_tools_used.join(", "))),
    );
    bindings.insert(
        "successful_tools_used".to_string(),
        VmValue::String(Rc::from(state.successful_tools_used.join(", "))),
    );
    bindings.insert(
        "transcript".to_string(),
        VmValue::String(Rc::from(transcript)),
    );
    bindings.insert(
        "last_text".to_string(),
        VmValue::String(Rc::from(last_text.trim().to_string())),
    );
    crate::stdlib::template::render_stdlib_prompt_asset(
        "agent/prompts/completion_judge_user.harn.prompt",
        Some(&bindings),
    )
    .unwrap_or_else(|error| format!("completion judge user prompt render error: {error}"))
}

fn render_judge_transcript(state: &AgentLoopState) -> String {
    let messages = if state.visible_messages.is_empty() {
        &state.recorded_messages
    } else {
        &state.visible_messages
    };
    let mut entries: Vec<(usize, String)> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (index, render_judge_message(index, message)))
        .collect();
    if entries.is_empty() {
        return state
            .transcript_summary
            .as_deref()
            .filter(|summary| !summary.trim().is_empty())
            .unwrap_or("<empty transcript>")
            .to_string();
    }

    let mut selected: Vec<(usize, String)> = entries.drain(..entries.len().min(4)).collect();
    let mut used_chars: usize = selected
        .iter()
        .map(|(_, entry)| entry.chars().count())
        .sum();
    if let Some(summary) = state
        .transcript_summary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
    {
        used_chars = used_chars.saturating_add(summary.chars().count());
    }

    let first_tail_index = selected
        .last()
        .map(|(index, _)| index.saturating_add(1))
        .unwrap_or(0);
    let mut tail: Vec<(usize, String)> = Vec::new();
    for (index, entry) in entries.into_iter().rev() {
        if index < first_tail_index {
            continue;
        }
        let entry_chars = entry.chars().count();
        if used_chars.saturating_add(entry_chars) > MAX_JUDGE_TRANSCRIPT_CHARS {
            break;
        }
        used_chars = used_chars.saturating_add(entry_chars);
        tail.push((index, entry));
    }
    tail.reverse();

    let omitted_count = match (selected.last(), tail.first()) {
        (Some((last_head_index, _)), Some((first_tail_index, _)))
            if first_tail_index > last_head_index =>
        {
            first_tail_index.saturating_sub(last_head_index.saturating_add(1))
        }
        (Some((last_head_index, _)), None) => messages.len().saturating_sub(last_head_index + 1),
        _ => 0,
    };
    if omitted_count > 0 {
        selected.push((
            first_tail_index,
            format!("[... omitted {omitted_count} middle transcript message(s) ...]"),
        ));
    }
    selected.extend(tail);

    let mut output = String::new();
    if let Some(summary) = state
        .transcript_summary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
    {
        output.push_str("Compacted transcript summary:\n");
        output.push_str(summary.trim());
        output.push_str("\n\n");
    }
    for (_, entry) in selected {
        output.push_str(&entry);
        output.push('\n');
    }
    output.trim_end().to_string()
}

fn render_judge_message(index: usize, message: &serde_json::Value) -> String {
    let role = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let name = message
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!(" name={name}"))
        .unwrap_or_default();
    let tool_call_id = message
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .filter(|tool_call_id| !tool_call_id.trim().is_empty())
        .map(|tool_call_id| format!(" tool_call_id={tool_call_id}"))
        .unwrap_or_default();
    let tool_calls = render_tool_calls_for_judge(message.get("tool_calls"));
    let content = message
        .get("content")
        .map(render_message_content_for_judge)
        .unwrap_or_default();
    format!(
        "[{index}] role={role}{name}{tool_call_id}{tool_calls}\n{}",
        truncate_chars(content.trim(), MAX_JUDGE_MESSAGE_CHARS)
    )
}

fn render_tool_calls_for_judge(tool_calls: Option<&serde_json::Value>) -> String {
    let Some(tool_calls) = tool_calls.and_then(serde_json::Value::as_array) else {
        return String::new();
    };
    if tool_calls.is_empty() {
        return String::new();
    }
    let names: Vec<String> = tool_calls
        .iter()
        .map(|tool_call| {
            tool_call
                .get("name")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    tool_call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("unknown")
                .to_string()
        })
        .collect();
    format!(" tool_calls={}", names.join(","))
}

fn render_message_content_for_judge(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push_str("\n[... truncated ...]");
    truncated
}

fn build_judge_options(
    config: &CompletionJudgeConfig,
    main_opts: &crate::llm::api::LlmCallOptions,
) -> BTreeMap<String, VmValue> {
    let mut options = config.options.clone();
    options
        .entry("system".to_string())
        .or_insert_with(|| VmValue::String(Rc::from(config.system.clone())));
    options
        .entry("provider".to_string())
        .or_insert_with(|| VmValue::String(Rc::from(main_opts.provider.clone())));
    options
        .entry("model".to_string())
        .or_insert_with(|| VmValue::String(Rc::from(main_opts.model.clone())));
    options
        .entry("temperature".to_string())
        .or_insert(VmValue::Float(0.0));
    options
        .entry("max_tokens".to_string())
        .or_insert(VmValue::Int(800));
    options
        .entry("schema_retries".to_string())
        .or_insert(VmValue::Int(1));
    options
        .entry("thinking".to_string())
        .or_insert(VmValue::Bool(false));
    options
}

async fn emit_judge_decision(
    session_id: &str,
    iteration: usize,
    verdict: &str,
    reasoning: &str,
    next_step: Option<String>,
    judge_duration_ms: u64,
) {
    emit_agent_event(&AgentEvent::JudgeDecision {
        session_id: session_id.to_string(),
        iteration,
        verdict: verdict.to_string(),
        reasoning: reasoning.to_string(),
        next_step,
        judge_duration_ms,
    })
    .await;
}

async fn inject_judge_feedback(
    state: &mut AgentLoopState,
    session_id: &str,
    feedback_kind: &str,
    feedback: &str,
) {
    let message = runtime_feedback_message(feedback_kind, feedback);
    append_message_to_contexts(
        &mut state.visible_messages,
        &mut state.recorded_messages,
        message,
    );
    emit_agent_event(&AgentEvent::FeedbackInjected {
        session_id: session_id.to_string(),
        kind: feedback_kind.to_string(),
        content: feedback.to_string(),
    })
    .await;
}
