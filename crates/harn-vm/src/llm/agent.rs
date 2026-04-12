use std::rc::Rc;

use serde::Deserialize;

use crate::value::{ErrorCategory, VmError, VmValue};

use super::daemon::{
    detect_watch_changes, load_snapshot, persist_snapshot, watch_state, DaemonSnapshot,
};
use super::helpers::{transcript_event, transcript_to_vm_with_events};
use super::tools::{
    build_assistant_tool_message, build_tool_calling_contract_prompt, build_tool_result_message,
    collect_tool_schemas, normalize_tool_args, parse_text_tool_calls_with_tools,
    validate_tool_args,
};

// Imports from extracted submodules.
use super::agent_config::{parse_post_turn_directive, AgentLoopConfig};
use super::agent_observe::{dump_llm_interpreted_response, observed_llm_call, LlmRetryConfig};
use super::agent_tools::{
    classify_tool_mutation, declared_paths, denied_tool_result, dispatch_tool_execution,
    is_denied_tool_result, is_read_only_tool, loop_intervention_message,
    merge_agent_loop_approval_policy, merge_agent_loop_policy, native_protocol_violation_nudge,
    normalize_native_tools_for_format, normalize_tool_choice_for_format,
    normalize_tool_examples_for_format, render_tool_result, stable_hash, stable_hash_str,
    LoopIntervention, ToolCallTracker,
};
use super::daemon::DaemonLoopConfig;

// Re-export items that moved to submodules so agent_tests.rs can use `super::`.
#[cfg(test)]
pub(super) use super::agent_config::build_llm_call_result;
#[cfg(test)]
pub(super) use super::agent_observe::extract_retry_after_ms;
#[cfg(test)]
pub(super) use super::agent_tools::required_tool_choice_for_provider;

thread_local! {
    static CURRENT_HOST_BRIDGE: std::cell::RefCell<Option<Rc<crate::bridge::HostBridge>>> = const { std::cell::RefCell::new(None) };
}

fn loop_state_requests_phase_change(text: &str, current_phase: &str) -> bool {
    if current_phase.trim().is_empty() {
        return false;
    }

    let current_phase = current_phase.trim();
    let mut last_next_phase: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("next_phase:") {
            let phase = rest.trim();
            if !phase.is_empty() {
                last_next_phase = Some(phase);
            }
        }
    }

    last_next_phase.is_some_and(|phase| phase != current_phase)
}

fn should_stop_after_successful_tools(
    tool_results: &[serde_json::Value],
    stop_tools: &[String],
) -> bool {
    has_successful_tools(tool_results, stop_tools)
}

fn has_successful_tools(tool_results: &[serde_json::Value], tool_names: &[String]) -> bool {
    tool_results
        .iter()
        .filter(|result| result["status"].as_str() == Some("ok"))
        .filter_map(|result| result["tool_name"].as_str())
        .any(|tool_name| tool_names.iter().any(|wanted| wanted == tool_name))
}

fn prose_char_len(text: &str) -> usize {
    text.trim().chars().count()
}

fn prose_exceeds_budget(
    prose: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> bool {
    let Some(limit) = turn_policy.and_then(|policy| policy.max_prose_chars) else {
        return false;
    };
    prose_char_len(prose) > limit
}

fn trim_prose_for_history(
    prose: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> String {
    let trimmed = prose.trim();
    let Some(limit) = turn_policy.and_then(|policy| policy.max_prose_chars) else {
        return trimmed.to_string();
    };
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        return trimmed.to_string();
    }
    let kept: String = chars.into_iter().take(limit).collect();
    format!("{kept}\n\n<assistant prose truncated by turn policy; keep prose brief and act>")
}

pub(super) fn action_turn_nudge(
    tool_format: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
    prose_too_long: bool,
) -> Option<String> {
    let policy = turn_policy?;
    if !policy.require_action_or_yield {
        return None;
    }
    let prose_clause = if let Some(limit) = policy.max_prose_chars {
        format!("Keep prose to at most {limit} visible characters, then")
    } else {
        "Keep prose brief, then".to_string()
    };
    let emphasis = if prose_too_long {
        " Your last response spent too much budget on prose."
    } else {
        ""
    };
    let completion_clause = if policy.allow_done_sentinel {
        "either call at least one tool, switch phase, or output the done sentinel if the task is genuinely complete."
    } else {
        "either call at least one tool or switch phase if the workflow allows it."
    };
    let mode_clause = if tool_format == "native" {
        " Use the provider tool channel only; handwritten tool-call text is invalid in this transcript."
    } else {
        ""
    };
    Some(format!(
        "{prose_clause} {completion_clause}{emphasis}{mode_clause}"
    ))
}

fn sentinel_without_action_nudge(
    tool_format: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> String {
    let mut message = if turn_policy.is_some_and(|policy| !policy.allow_done_sentinel) {
        "You emitted a done sentinel in a workflow-owned action stage. The task is not complete yet. Use an available tool now, or switch phase if the workflow allows it. Do not output a done sentinel in this stage.".to_string()
    } else {
        "You emitted the done sentinel without taking any tool action. The task is not complete yet. Use an available tool now, or switch phase if the workflow allows it. Do not output the done sentinel again until you have acted.".to_string()
    };
    if let Some(nudge) = action_turn_nudge(tool_format, turn_policy, false) {
        message.push(' ');
        message.push_str(&nudge);
    }
    message
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

async fn inject_queued_user_messages(
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    messages: &mut Vec<serde_json::Value>,
    checkpoint: crate::bridge::DeliveryCheckpoint,
) -> Result<Vec<crate::bridge::QueuedUserMessage>, VmError> {
    let Some(bridge) = bridge else {
        return Ok(Vec::new());
    };
    let queued = bridge.take_queued_user_messages_for(checkpoint).await;
    for message in &queued {
        messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
    Ok(queued)
}

fn append_message_to_contexts(
    visible_messages: &mut Vec<serde_json::Value>,
    recorded_messages: &mut Vec<serde_json::Value>,
    message: serde_json::Value,
) {
    visible_messages.push(message.clone());
    recorded_messages.push(message);
}

/// Replacement text for an assistant turn whose ```call blocks all failed to
/// parse. Feeding the raw malformed text back into the next request causes the
/// model to mutate its own broken syntax (observed self-poison loop in
/// local-gemma4 eval 2026-04-05). We keep the parse-error diagnostic as the
/// user-role reply, but elide the broken assistant content from history.
fn compact_malformed_assistant_turn(error_count: usize) -> String {
    let plural = if error_count == 1 { "" } else { "s" };
    format!(
        "<assistant turn elided: produced {error_count} malformed tool call{plural} \
         (see parse error below). Emit a corrected call in the next turn.>"
    )
}

fn append_host_messages_to_recorded(
    recorded_messages: &mut Vec<serde_json::Value>,
    queued_messages: &[crate::bridge::QueuedUserMessage],
) {
    for message in queued_messages {
        recorded_messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
}

fn build_agent_system_prompt(
    base_system: Option<&str>,
    tool_prompt: Option<&str>,
    persistent_prompt: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(base) = base_system {
        let trimmed = base.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(tool_prompt) = tool_prompt {
        let trimmed = tool_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(persistent_prompt) = persistent_prompt {
        let trimmed = persistent_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

async fn maybe_auto_compact_agent_messages(
    opts: &super::api::LlmCallOptions,
    auto_compact: &Option<crate::orchestration::AutoCompactConfig>,
    visible_messages: &mut Vec<serde_json::Value>,
    transcript_summary: &mut Option<String>,
) -> Result<(), VmError> {
    if let Some(ac) = auto_compact {
        let approx_tokens = crate::orchestration::estimate_message_tokens(visible_messages);
        if approx_tokens >= ac.token_threshold {
            let mut compact_opts = opts.clone();
            compact_opts.messages = visible_messages.clone();
            if let Some(summary) = crate::orchestration::auto_compact_messages(
                visible_messages,
                ac,
                Some(&compact_opts),
            )
            .await?
            {
                let merged = match transcript_summary.take() {
                    Some(existing) if !existing.is_empty() => {
                        format!("{existing}\n\n{summary}")
                    }
                    _ => summary,
                };
                *transcript_summary = Some(merged);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn daemon_snapshot_from_state(
    daemon_state: &str,
    visible_messages: &[serde_json::Value],
    recorded_messages: &[serde_json::Value],
    transcript_summary: &Option<String>,
    transcript_events: &[VmValue],
    total_text: &str,
    last_iteration_text: &str,
    all_tools_used: &[String],
    rejected_tools: &[String],
    deferred_user_messages: &[String],
    total_iterations: usize,
    idle_backoff_ms: u64,
    last_run_exit_code: Option<i32>,
    watch_state_map: &std::collections::BTreeMap<String, u64>,
) -> DaemonSnapshot {
    DaemonSnapshot {
        daemon_state: daemon_state.to_string(),
        visible_messages: visible_messages.to_vec(),
        recorded_messages: recorded_messages.to_vec(),
        transcript_summary: transcript_summary.clone(),
        transcript_events: transcript_events
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
        total_text: total_text.to_string(),
        last_iteration_text: last_iteration_text.to_string(),
        all_tools_used: all_tools_used.to_vec(),
        rejected_tools: rejected_tools.to_vec(),
        deferred_user_messages: deferred_user_messages.to_vec(),
        total_iterations,
        idle_backoff_ms,
        last_run_exit_code,
        watch_state: watch_state_map.clone(),
        ..Default::default()
    }
}

fn maybe_persist_daemon_snapshot(
    config: &DaemonLoopConfig,
    snapshot: &DaemonSnapshot,
) -> Result<Option<String>, VmError> {
    let Some(path) = config.effective_persist_path() else {
        return Ok(None);
    };
    persist_snapshot(path, snapshot).map(Some)
}

#[derive(Debug, Deserialize)]
struct AgentContextCallbackResponse {
    #[serde(default)]
    messages: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    system: Option<String>,
}

fn message_content_text(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let items = content.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        let item_type = item
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match item_type {
            "text" => {
                if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            "tool_result" => {
                if let Some(text) = item.get("content").and_then(|value| value.as_str()) {
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn latest_message_text(messages: &[serde_json::Value], role: &str) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(|value| value.as_str()) == Some(role))
        .and_then(message_content_text)
}

fn latest_tool_result_text(messages: &[serde_json::Value]) -> Option<String> {
    for message in messages.iter().rev() {
        let role = message
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if role == "tool" {
            if let Some(text) = message_content_text(message) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            continue;
        }
        if role != "user" {
            continue;
        }
        let Some(items) = message.get("content").and_then(|value| value.as_array()) else {
            continue;
        };
        for item in items {
            if item.get("type").and_then(|value| value.as_str()) != Some("tool_result") {
                continue;
            }
            if let Some(text) = item.get("content").and_then(|value| value.as_str()) {
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

fn recent_message_tail(messages: &[serde_json::Value], count: usize) -> Vec<serde_json::Value> {
    let len = messages.len();
    let start = len.saturating_sub(count);
    messages[start..].to_vec()
}

async fn apply_agent_context_callback(
    callback: &VmValue,
    iteration: usize,
    system: Option<&str>,
    visible_messages: &[serde_json::Value],
    recorded_messages: &[serde_json::Value],
) -> Result<(Vec<serde_json::Value>, Option<String>), VmError> {
    let Some(VmValue::Closure(closure)) = Some(callback.clone()) else {
        return Err(VmError::Runtime(
            "context_callback must be a closure".to_string(),
        ));
    };
    let payload = serde_json::json!({
        "iteration": iteration,
        "system": system,
        "messages": visible_messages,
        "visible_messages": visible_messages,
        "recorded_messages": recorded_messages,
        "recent_visible_messages": recent_message_tail(visible_messages, 8),
        "recent_recorded_messages": recent_message_tail(recorded_messages, 12),
        "latest_visible_user_message": latest_message_text(visible_messages, "user"),
        "latest_visible_assistant_message": latest_message_text(visible_messages, "assistant"),
        "latest_recorded_user_message": latest_message_text(recorded_messages, "user"),
        "latest_recorded_assistant_message": latest_message_text(recorded_messages, "assistant"),
        "latest_tool_result": latest_tool_result_text(visible_messages),
        "latest_recorded_tool_result": latest_tool_result_text(recorded_messages),
    });
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("context_callback requires an async builtin VM context".to_string())
    })?;
    let payload_vm = crate::stdlib::json_to_vm_value(&payload);
    let result = vm.call_closure_pub(&closure, &[payload_vm], &[]).await;
    let value = result?;
    match value {
        VmValue::Nil => Ok((visible_messages.to_vec(), system.map(|s| s.to_string()))),
        VmValue::List(list) => Ok((
            list.iter().map(crate::llm::helpers::vm_value_to_json).collect(),
            system.map(|s| s.to_string()),
        )),
        VmValue::Dict(_) => {
            let parsed: AgentContextCallbackResponse =
                serde_json::from_value(crate::llm::helpers::vm_value_to_json(&value)).map_err(
                    |error| {
                        VmError::Runtime(format!(
                            "context_callback returned an invalid response: {error}"
                        ))
                    },
                )?;
            Ok((
                parsed.messages.unwrap_or_else(|| visible_messages.to_vec()),
                parsed.system.or_else(|| system.map(|s| s.to_string())),
            ))
        }
        other => Err(VmError::Runtime(format!(
            "context_callback must return nil, a messages list, or a dict with optional messages/system fields; got {}",
            other.display()
        ))),
    }
}

pub async fn run_agent_loop_internal(
    opts: &mut super::api::LlmCallOptions,
    config: AgentLoopConfig,
) -> Result<serde_json::Value, VmError> {
    struct ExecutionPolicyGuard {
        active: bool,
    }

    impl Drop for ExecutionPolicyGuard {
        fn drop(&mut self) {
            if self.active {
                crate::orchestration::pop_execution_policy();
            }
        }
    }

    struct ApprovalPolicyGuard {
        active: bool,
    }

    impl Drop for ApprovalPolicyGuard {
        fn drop(&mut self) {
            if self.active {
                crate::orchestration::pop_approval_policy();
            }
        }
    }

    let bridge = current_host_bridge();
    let max_iterations = config.max_iterations;
    let persistent = config.persistent;
    let max_nudges = config.max_nudges;
    let custom_nudge = config.nudge;
    let done_sentinel = config
        .done_sentinel
        .clone()
        .unwrap_or_else(|| "##DONE##".to_string());
    let break_unless_phase = config.break_unless_phase.clone();
    let tool_retries = config.tool_retries;
    let tool_backoff_ms = config.tool_backoff_ms;
    let tool_format = config.tool_format;
    let context_callback = config.context_callback.clone();

    let auto_compact = config.auto_compact.clone();
    let daemon = config.daemon;
    let daemon_config = config.daemon_config.clone();
    let exit_when_verified = config.exit_when_verified;
    let mut last_run_exit_code: Option<i32> = None;

    // Tool loop detection — catches stuck loops where the model calls the
    // same tool with the same args and gets the same result repeatedly.
    let loop_detect_enabled = config.loop_detect_warn > 0;
    let mut loop_tracker = ToolCallTracker::new(
        config.loop_detect_warn,
        config.loop_detect_block,
        config.loop_detect_skip,
    );

    let effective_policy = merge_agent_loop_policy(config.policy.clone())?;

    // Push the loop-local policy only after intersecting it with any active
    // outer workflow/worker ceiling so nested loops never widen permissions.
    if let Some(ref policy) = effective_policy {
        crate::orchestration::push_execution_policy(policy.clone());
    }
    let _policy_guard = ExecutionPolicyGuard {
        active: effective_policy.is_some(),
    };

    let effective_approval_policy =
        merge_agent_loop_approval_policy(config.approval_policy.clone());
    if let Some(ref policy) = effective_approval_policy {
        crate::orchestration::push_approval_policy(policy.clone());
    }
    let _approval_guard = ApprovalPolicyGuard {
        active: effective_approval_policy.is_some(),
    };

    let tools_owned = opts.tools.clone();
    let tools_val = tools_owned.as_ref();
    opts.native_tools = normalize_native_tools_for_format(&tool_format, opts.native_tools.clone());
    opts.tool_choice = normalize_tool_choice_for_format(
        &opts.provider,
        &tool_format,
        opts.native_tools.as_deref(),
        opts.tool_choice.clone(),
        config.turn_policy.as_ref(),
    );
    let native_tools_for_prompt = opts.native_tools.clone();
    let rendered_schemas =
        crate::llm::tools::collect_tool_schemas(tools_val, native_tools_for_prompt.as_deref());
    let has_tools = !rendered_schemas.is_empty();
    let base_system = opts.system.clone();
    let tool_examples =
        normalize_tool_examples_for_format(&tool_format, config.tool_examples.clone());
    let tool_contract_prompt = if has_tools {
        Some(build_tool_calling_contract_prompt(
            tools_val,
            native_tools_for_prompt.as_deref(),
            &tool_format,
            config
                .turn_policy
                .as_ref()
                .is_some_and(|policy| policy.require_action_or_yield),
            tool_examples.as_deref(),
        ))
    } else {
        None
    };

    let allow_done_sentinel = config
        .turn_policy
        .as_ref()
        .map(|policy| policy.allow_done_sentinel)
        .unwrap_or(true);
    let persistent_system_prompt = if persistent {
        if exit_when_verified {
            // When exit_when_verified is set, the harness enforces that the
            // done sentinel is only honoured after a passing run(). The
            // system prompt only needs a brief reminder, not a long rule.
            if allow_done_sentinel {
                Some(format!(
                    "\n\nKeep working until the task is complete. Take action with tools — \
                     do not stop to explain. Output {done_sentinel} when done."
                ))
            } else {
                Some(
                    "\n\nKeep working until the task is complete. Take action with tools — \
                     do not stop to explain."
                        .to_string(),
                )
            }
        } else {
            if allow_done_sentinel {
                Some(format!(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tools. \
                     When the requested work is complete, output {done_sentinel} on its own line."
                ))
            } else {
                Some(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tools."
                        .to_string(),
                )
            }
        }
    } else {
        None
    };
    let mut visible_messages = opts.messages.clone();
    let mut recorded_messages = opts.messages.clone();

    // `total_text` is the concatenation of every iteration's assistant text.
    // It is the raw transcript the reflector/meta-analysis callers want to
    // see end-to-end. It is NOT suitable as "the agent's answer" because
    // exploration turns and tool-call expressions from mid-run bleed into
    // it. Callers wanting "what the user should see" should use the
    // `visible_text` field instead, which is the LAST iteration's text
    // alone — see the Ok(json!) block at the end of this function.
    let mut total_text = String::new();
    let mut last_iteration_text = String::new();
    let mut consecutive_text_only = 0usize;
    let mut consecutive_single_tool_turns = 0usize;
    let mut all_tools_used: Vec<String> = Vec::new();
    let mut successful_tools_used: Vec<String> = Vec::new();
    let mut rejected_tools: Vec<String> = Vec::new();
    let mut deferred_user_messages: Vec<String> = Vec::new();
    let mut total_iterations = 0usize;
    let mut final_status = "done";
    let mut transcript_summary = opts.transcript_summary.clone();
    let loop_start = std::time::Instant::now();
    let mut transcript_events = Vec::new();
    let mut idle_backoff_ms = 100u64;
    let mut daemon_state = if daemon {
        "active".to_string()
    } else {
        "done".to_string()
    };
    let mut daemon_snapshot_path: Option<String> = None;
    let mut daemon_watch_state = watch_state(&daemon_config.watch_paths);
    let mut resumed_iterations = 0usize;

    if daemon {
        if let Some(path) = daemon_config.resume_path.as_deref() {
            let snapshot = load_snapshot(path)?;
            daemon_state = snapshot.daemon_state.clone();
            visible_messages = snapshot.visible_messages;
            recorded_messages = snapshot.recorded_messages;
            transcript_summary = snapshot.transcript_summary;
            transcript_events = snapshot
                .transcript_events
                .iter()
                .map(crate::stdlib::json_to_vm_value)
                .collect();
            total_text = snapshot.total_text;
            last_iteration_text = snapshot.last_iteration_text;
            all_tools_used = snapshot.all_tools_used;
            rejected_tools = snapshot.rejected_tools;
            deferred_user_messages = snapshot.deferred_user_messages;
            resumed_iterations = snapshot.total_iterations;
            total_iterations = resumed_iterations;
            idle_backoff_ms = snapshot.idle_backoff_ms.max(1);
            last_run_exit_code = snapshot.last_run_exit_code;
            daemon_watch_state = if snapshot.watch_state.is_empty() {
                watch_state(&daemon_config.watch_paths)
            } else {
                snapshot.watch_state
            };
            daemon_snapshot_path = Some(path.to_string());
        } else if let Some(path) = daemon_config.effective_persist_path() {
            daemon_snapshot_path = Some(path.to_string());
        }
    }

    for iteration in 0..max_iterations {
        total_iterations = resumed_iterations + iteration + 1;
        daemon_state = "active".to_string();
        let immediate_messages = inject_queued_user_messages(
            bridge.as_ref(),
            &mut visible_messages,
            crate::bridge::DeliveryCheckpoint::InterruptImmediate,
        )
        .await?;
        append_host_messages_to_recorded(&mut recorded_messages, &immediate_messages);
        for message in &immediate_messages {
            transcript_events.push(transcript_event(
                "host_input",
                "user",
                "public",
                &message.content,
                Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
            ));
        }
        if !immediate_messages.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
        }
        let default_system = build_agent_system_prompt(
            base_system.as_deref(),
            tool_contract_prompt.as_deref(),
            persistent_system_prompt.as_deref(),
        );
        let (call_messages, call_system) = if let Some(callback) = context_callback.as_ref() {
            apply_agent_context_callback(
                callback,
                iteration,
                default_system.as_deref(),
                &visible_messages,
                &recorded_messages,
            )
            .await?
        } else {
            (visible_messages.clone(), default_system)
        };
        crate::llm::api::debug_log_message_shapes(
            &format!("agent iteration={iteration} preflight"),
            &call_messages,
        );
        opts.messages = call_messages;
        opts.system = call_system;
        let result = observed_llm_call(
            opts,
            Some(&tool_format),
            bridge.as_ref(),
            &LlmRetryConfig {
                retries: config.llm_retries,
                backoff_ms: config.llm_backoff_ms,
            },
            Some(iteration),
            true,
            false, // agent_loop runs on the local set, not offthread
        )
        .await?;

        let text = result.text.clone();
        total_text.push_str(&text);
        // `last_iteration_text` is assigned below AFTER the tool-call parser
        // runs, so it holds the prose (calls stripped) rather than the raw
        // text. For the native-tool-call and no-tools branches we fall back
        // to the raw text a few lines down.
        transcript_events.push(transcript_event(
            "provider_payload",
            "assistant",
            "internal",
            "",
            Some(serde_json::json!({
                "model": result.model,
                "input_tokens": result.input_tokens,
                "output_tokens": result.output_tokens,
                "tool_calls": result.tool_calls,
                "tool_calling_mode": tool_format.clone(),
            })),
        ));
        if let Some(thinking) = result.thinking.clone() {
            if !thinking.is_empty() {
                transcript_events.push(transcript_event(
                    "private_reasoning",
                    "assistant",
                    "private",
                    &thinking,
                    None,
                ));
            }
        }

        let mut tool_parse_errors: Vec<String> = Vec::new();
        // `text_prose` is the model's text with any fenceless TS tool-call
        // expressions excised. When the parser doesn't run (native-format
        // path or no-tools path), it falls back to the raw text verbatim.
        let mut text_prose = text.clone();
        let tool_calls = if !result.tool_calls.is_empty() {
            result.tool_calls.clone()
        } else if has_tools {
            let parse_result = parse_text_tool_calls_with_tools(&text, tools_val);
            tool_parse_errors = parse_result.errors;
            text_prose = parse_result.prose;
            if tool_format == "native" {
                if !parse_result.calls.is_empty() || !tool_parse_errors.is_empty() {
                    let feedback = native_protocol_violation_nudge(
                        &tool_format,
                        config.turn_policy.as_ref(),
                        !parse_result.calls.is_empty(),
                    );
                    append_message_to_contexts(
                        &mut visible_messages,
                        &mut recorded_messages,
                        serde_json::json!({"role": "user", "content": feedback}),
                    );
                }
                Vec::new()
            } else {
                let calls = parse_result.calls;

                // When the parser found tool-call-looking text but couldn't
                // parse it, inject the specific parse error into the conversation
                // so the model knows what to fix (e.g. unescaped backtick inside
                // a template literal). Without this, the generic nudge gives the
                // model no signal about what was wrong.
                if calls.is_empty() && !tool_parse_errors.is_empty() {
                    let error_summary = tool_parse_errors
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; ");
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "{} tool-call parse error(s): {}",
                            tool_parse_errors.len(),
                            &error_summary[..error_summary.len().min(200)]
                        ),
                    );
                    let feedback = format!(
                        "Your tool call could not be parsed: {error_summary}\n\n\
                         Use heredoc syntax for multiline content — it requires NO escaping:\n\
                         edit({{\n\
                             action: \"create\",\n\
                             path: \"...\",\n\
                             content: <<EOF\n\
                         package main\n\
                         // backticks, quotes, backslashes — all fine inside heredoc\n\
                         EOF\n\
                         }})\n\n\
                         Do NOT use backtick template literals for code that contains \
                         backtick characters (Go raw strings, Rust raw strings, shell). \
                         Heredoc avoids all escaping issues."
                    );
                    append_message_to_contexts(
                        &mut visible_messages,
                        &mut recorded_messages,
                        serde_json::json!({"role": "user", "content": feedback}),
                    );
                }
                calls
            }
        } else {
            Vec::new()
        };
        let prose_too_long = prose_exceeds_budget(&text_prose, config.turn_policy.as_ref());
        let shaped_text_prose = trim_prose_for_history(&text_prose, config.turn_policy.as_ref());
        let interpreted_call_id = format!("iteration-{iteration}");
        dump_llm_interpreted_response(
            iteration,
            &interpreted_call_id,
            &tool_format,
            &shaped_text_prose,
            &tool_calls,
            &tool_parse_errors,
        );
        // Surface the prose (not the raw text) to callers that read
        // `last_iteration_text` / `visible_text`. Tool call expressions are
        // structured data in `tool_calls`, not something the user should
        // see as the agent's "answer". This also means conversation history
        // will carry the prose, so future iterations don't see their own
        // prior call syntax as narration.
        last_iteration_text = shaped_text_prose.clone();

        // Check done_sentinel on EVERY response, not just text-only ones.
        // If present alongside tool calls, we still process the tools (so their
        // results land in the conversation), but mark the loop to exit afterward.
        let sentinel_in_text = text.contains(&done_sentinel);
        let phase_change = break_unless_phase
            .as_deref()
            .is_some_and(|phase| loop_state_requests_phase_change(&text, phase));
        if phase_change {
            if let Some(ref phase) = break_unless_phase {
                super::trace::emit_agent_event(super::trace::AgentTraceEvent::PhaseChange {
                    from_phase: phase.clone(),
                    to_phase: text
                        .lines()
                        .rev()
                        .find_map(|l| l.trim().strip_prefix("next_phase:"))
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    iteration,
                });
            }
        }
        // When exit_when_verified is set, the sentinel is only honoured if the
        // last run() tool call returned exit code 0.  This prevents premature
        // exit when the model claims it's done but verification hasn't passed.
        let verified = !exit_when_verified || last_run_exit_code == Some(0);
        // Guard: the model must have made at least one tool call before the
        // done sentinel is honoured.  This prevents premature exits where the
        // model describes a plan and emits ##DONE## without actually acting.
        let has_acted = !all_tools_used.is_empty() || !tool_calls.is_empty();
        let sentinel_hit =
            persistent && ((sentinel_in_text && verified && has_acted) || phase_change);

        // If the model emitted the sentinel but verification hasn't passed,
        // inject a corrective so the model knows it must keep going.
        if sentinel_in_text && !verified && persistent {
            let code_str = last_run_exit_code.map_or("none".to_string(), |c| c.to_string());
            let corrective = format!(
                "You emitted the done sentinel but verification has not passed \
                 (last run exit code: {code_str}). The loop will continue. \
                 Run the verification command and fix any failures before finishing."
            );
            visible_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
            recorded_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
        }
        // If the model emitted the sentinel without having made any tool
        // calls, it's trying to declare done without doing any work.
        if sentinel_in_text && !has_acted && persistent && has_tools {
            let corrective =
                sentinel_without_action_nudge(&tool_format, config.turn_policy.as_ref());
            visible_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
            recorded_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
        }

        if !tool_calls.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
            if tool_format == "native" {
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    build_assistant_tool_message(&text, &tool_calls, &opts.provider),
                );
            } else {
                // When some calls parsed but others didn't, replace the raw
                // assistant text with a compact summary so the next iteration
                // cannot see (and mutate) the malformed call blocks. Tool
                // results for the successful calls are appended below, so the
                // model still has their outcomes.
                // In the clean case we append the model's PROSE (tool-call
                // expressions stripped). Tool calls are structured data —
                // replaying them as narration would make the next turn see
                // them as both invocations AND narration commentary, which
                // is exactly the "visible_text leaked lookup()/read() into
                // the refined prompt" bug we saw in the rewriter.
                let assistant_content_for_history = if tool_parse_errors.is_empty() {
                    shaped_text_prose.clone()
                } else {
                    format!(
                        "<assistant turn partially elided: {} tool call(s) executed successfully \
                         ({}), {} malformed tool call(s) rejected. \
                         See tool results and parse errors that follow.>",
                        tool_calls.len(),
                        tool_calls
                            .iter()
                            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                            .collect::<Vec<_>>()
                            .join(", "),
                        tool_parse_errors.len(),
                    )
                };
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "assistant",
                        "content": assistant_content_for_history,
                    }),
                );
            }

            let mut observations = String::new();
            let mut tools_used_this_iter = Vec::new();
            let mut tool_results_this_iter: Vec<serde_json::Value> = Vec::new();
            let mut rejection_followups: Vec<String> = Vec::new();
            let tool_schemas = collect_tool_schemas(tools_val, opts.native_tools.as_deref());

            // Parallel dispatch for read-only exploration batches. When the
            // leading run of tool calls in this assistant response are all in
            // the read-only set (read, lookup, search, outline, list_templates,
            // get_template, web_search, web_fetch), we concurrently pre-fetch
            // their execution results via join_all. This covers two cases:
            //   (a) all tools read-only — entire turn runs in parallel latency
            //   (b) mixed turn starting with reads — the read-only prefix
            //       runs in parallel, then sequential dispatch handles the
            //       non-read-only tail (edit, run, etc.) as before.
            //
            // The sequential loop still runs for ALL bookkeeping (policy
            // checks, hooks, transcript events, observation appending,
            // post-hooks, ordering) — only the actual tool-execution step is
            // parallelized, and only for tools whose index is in the cache.
            // Any hook denial or arg mutation falls through to the sequential
            // path, which safely recomputes that single call.
            let ro_prefix_len: usize = tool_calls
                .iter()
                .position(|tc| !is_read_only_tool(tc["name"].as_str().unwrap_or("")))
                .unwrap_or(tool_calls.len());
            let parallel_indices: Vec<usize> = if ro_prefix_len >= 2 {
                (0..ro_prefix_len).collect()
            } else {
                Vec::new()
            };
            let mut parallel_results: std::collections::HashMap<
                usize,
                Result<serde_json::Value, VmError>,
            > = std::collections::HashMap::new();
            if !parallel_indices.is_empty() {
                // Build futures for each read-only execution. We use the raw
                // tool_args here (pre-hook); if a hook would modify or deny,
                // the sequential loop will still run its full checks and
                // choose to either reuse our result (if hooks are Allow with
                // no modifications) or recompute. This is safe because we
                // only cache results for read-only tools which have no side
                // effects — re-running them is at worst wasted work.
                use futures::future::join_all;
                let futures = parallel_indices.iter().map(|&idx| {
                    let tc = tool_calls[idx].clone();
                    let tool_name = tc["name"].as_str().unwrap_or("").to_string();
                    let tool_args = normalize_tool_args(&tool_name, &tc["arguments"]);
                    let tool_retries_local = tool_retries;
                    let tool_backoff_ms_local = tool_backoff_ms;
                    let bridge_local = bridge.clone();
                    let tools_val_local = tools_val.cloned();
                    async move {
                        dispatch_tool_execution(
                            &tool_name,
                            &tool_args,
                            tools_val_local.as_ref(),
                            bridge_local.as_ref(),
                            tool_retries_local,
                            tool_backoff_ms_local,
                        )
                        .await
                    }
                });
                let joined: Vec<Result<serde_json::Value, VmError>> = join_all(futures).await;
                for (i, idx) in parallel_indices.iter().enumerate() {
                    parallel_results.insert(*idx, joined[i].clone());
                }
            }

            for (tc_index, tc) in tool_calls.iter().enumerate() {
                let tool_id = tc["id"].as_str().unwrap_or("");
                let tool_name = tc["name"].as_str().unwrap_or("");
                let mut tool_args = normalize_tool_args(tool_name, &tc["arguments"]);

                // Detect malformed JSON arguments that the provider returned
                // (marked with __parse_error sentinel during response parsing).
                if let Some(parse_err) = tool_args.get("__parse_error").and_then(|v| v.as_str()) {
                    let result_text = format!("ERROR: {parse_err}");
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                let policy_result = crate::orchestration::enforce_current_policy_for_tool(
                    tool_name,
                )
                .and_then(|_| {
                    crate::orchestration::enforce_tool_arg_constraints(
                        &crate::orchestration::current_execution_policy().unwrap_or_default(),
                        tool_name,
                        &tool_args,
                    )
                });
                if let Err(error) = policy_result {
                    let result_text = render_tool_result(&denied_tool_result(
                        tool_name,
                        format!(
                            "{error}. Use one of the declared tools exactly as named and put extra fields inside that tool's arguments."
                        ),
                    ));
                    if !rejected_tools.contains(&tool_name.to_string()) {
                        rejected_tools.push(tool_name.to_string());
                    }
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                // Declarative approval policy: auto-approve / auto-deny / require host.
                let approval_decision = crate::orchestration::current_approval_policy()
                    .map(|policy| policy.evaluate(tool_name, &tool_args));
                let approval_outcome = match approval_decision {
                    None | Some(crate::orchestration::ToolApprovalDecision::AutoApproved) => {
                        Ok(None)
                    }
                    Some(crate::orchestration::ToolApprovalDecision::AutoDenied { reason }) => {
                        Err(("auto_denied", reason))
                    }
                    Some(crate::orchestration::ToolApprovalDecision::RequiresHostApproval) => {
                        if let Some(bridge) = bridge.as_ref() {
                            let mutation = crate::orchestration::current_mutation_session();
                            let payload = serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "args": tool_args,
                                "mutation": mutation,
                                "declared_paths": declared_paths(tool_name, &tool_args),
                            });
                            match bridge.call("tool/request_approval", payload).await {
                                Ok(response) => {
                                    let granted = response
                                        .get("granted")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    if granted {
                                        if let Some(new_args) = response.get("args") {
                                            tool_args = new_args.clone();
                                        }
                                        Ok(Some("host_granted"))
                                    } else {
                                        let reason = response
                                            .get("reason")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("host did not grant approval")
                                            .to_string();
                                        Err(("host_denied", reason))
                                    }
                                }
                                Err(_) => Err((
                                    "host_denied",
                                    "approval request failed or host does not implement \
                                     tool/request_approval"
                                        .to_string(),
                                )),
                            }
                        } else {
                            Err((
                                "host_denied",
                                "approval required but no host bridge is available".to_string(),
                            ))
                        }
                    }
                };
                if let Err((approval_status, reason)) = approval_outcome {
                    let result_text = render_tool_result(&denied_tool_result(tool_name, reason));
                    if !rejected_tools.contains(&tool_name.to_string()) {
                        rejected_tools.push(tool_name.to_string());
                    }
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                            "approval": approval_status,
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }
                if let Ok(Some(approval_status)) = approval_outcome {
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        "",
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "approval": approval_status,
                        })),
                    ));
                }

                // PreToolUse hooks: in-process hooks first, then bridge gate
                match crate::orchestration::run_pre_tool_hooks(tool_name, &tool_args) {
                    crate::orchestration::PreToolAction::Allow => {}
                    crate::orchestration::PreToolAction::Deny(reason) => {
                        let result_text =
                            render_tool_result(&denied_tool_result(tool_name, reason));
                        if !rejected_tools.contains(&tool_name.to_string()) {
                            rejected_tools.push(tool_name.to_string());
                        }
                        transcript_events.push(transcript_event(
                            "tool_execution", "tool", "internal", &result_text,
                            Some(serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true})),
                        ));
                        if tool_format == "native" {
                            append_message_to_contexts(
                                &mut visible_messages,
                                &mut recorded_messages,
                                build_tool_result_message(tool_id, &result_text, &opts.provider),
                            );
                        } else {
                            observations.push_str(&format!(
                                "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                            ));
                        }
                        continue;
                    }
                    crate::orchestration::PreToolAction::Modify(new_args) => {
                        tool_args = new_args;
                    }
                }

                // on_tool_call closure hook (Harn-level pre-execution).
                // For declarative deny/approve, use `approval_policy` on the agent loop.
                // This hook remains for arg rewriting before dispatch.
                if let Some(VmValue::Closure(ref closure)) = config.on_tool_call {
                    if let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() {
                        let hook_arg = crate::stdlib::json_to_vm_value(&serde_json::json!({
                            "tool_name": tool_name,
                            "args": tool_args,
                        }));
                        if let Ok(result) = vm.call_closure_pub(closure, &[hook_arg], &[]).await {
                            if let Some(dict) = result.as_dict() {
                                if let Some(new_args) = dict.get("args") {
                                    tool_args = crate::llm::vm_value_to_json(new_args);
                                }
                            }
                        }
                    }
                }

                // Bridge-level PreToolUse gate: host can allow/deny/modify
                if let Some(bridge) = bridge.as_ref() {
                    let mutation = crate::orchestration::current_mutation_session();
                    let mutation_classification = classify_tool_mutation(tool_name);
                    if let Ok(response) = bridge
                        .call(
                            "tool/pre_use",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "args": tool_args,
                                "mutation": {
                                    "classification": mutation_classification,
                                    "session": mutation,
                                    "declared_paths": declared_paths(tool_name, &tool_args),
                                },
                            }),
                        )
                        .await
                    {
                        let action = response
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("allow");
                        match action {
                            "deny" => {
                                let reason = response
                                    .get("reason")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("denied by host");
                                let result_text =
                                    render_tool_result(&denied_tool_result(tool_name, reason));
                                rejection_followups.push(format!(
                                    "The previous tool call was rejected by the host. Treat this as a hard instruction and follow it exactly now.\n{reason}"
                                ));
                                if !rejected_tools.contains(&tool_name.to_string()) {
                                    rejected_tools.push(tool_name.to_string());
                                }
                                transcript_events.push(transcript_event(
                                    "tool_execution", "tool", "internal", &result_text,
                                    Some(serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true})),
                                ));
                                if tool_format == "native" {
                                    append_message_to_contexts(
                                        &mut visible_messages,
                                        &mut recorded_messages,
                                        build_tool_result_message(
                                            tool_id,
                                            &result_text,
                                            &opts.provider,
                                        ),
                                    );
                                } else {
                                    observations.push_str(&format!(
                                        "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                                    ));
                                }
                                continue;
                            }
                            "modify" => {
                                if let Some(new_args) = response.get("args") {
                                    tool_args = new_args.clone();
                                }
                            }
                            _ => {} // "allow" or anything else — proceed
                        }
                    }
                    // If the bridge call fails (host doesn't implement tool/pre_use),
                    // we silently proceed — the host can opt in to this protocol.
                }
                // Validate required parameters before dispatch so the LLM gets
                // a clear error instead of a cryptic handler failure.
                if let Err(msg) = validate_tool_args(tool_name, &tool_args, &tool_schemas) {
                    let result_text = format!("ERROR: {msg}");
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                transcript_events.push(transcript_event(
                    "tool_intent",
                    "assistant",
                    "internal",
                    tool_name,
                    Some(
                        serde_json::json!({"arguments": tool_args.clone(), "tool_use_id": tool_id}),
                    ),
                ));
                tools_used_this_iter.push(tool_name.to_string());
                let mutation_classification = classify_tool_mutation(tool_name);
                let declared_paths_current = declared_paths(tool_name, &tool_args);
                let tool_started_at = std::time::Instant::now();
                let tool_call_id = if tool_id.is_empty() {
                    format!("tool-iter-{iteration}-{}", tools_used_this_iter.len())
                } else {
                    format!("tool-{tool_id}")
                };
                let tool_span_id = crate::tracing::span_start(
                    crate::tracing::SpanKind::ToolCall,
                    tool_name.to_string(),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "tool_name",
                    serde_json::json!(tool_name),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "tool_use_id",
                    serde_json::json!(tool_id),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "call_id",
                    serde_json::json!(tool_call_id.clone()),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "iteration",
                    serde_json::json!(iteration),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "classification",
                    serde_json::json!(mutation_classification.clone()),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "declared_paths",
                    serde_json::json!(declared_paths_current.clone()),
                );
                if let Some(bridge) = bridge.as_ref() {
                    bridge.send_call_start(
                        &tool_call_id,
                        "tool",
                        tool_name,
                        serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "iteration": iteration,
                            "classification": mutation_classification,
                            "declared_paths": declared_paths_current,
                        }),
                    );
                }

                // Tool loop detection: check BEFORE dispatch whether this
                // exact call has been stuck in a loop.
                let args_hash = if loop_detect_enabled {
                    stable_hash(&tool_args)
                } else {
                    0
                };
                if loop_detect_enabled {
                    if let LoopIntervention::Skip { count } =
                        loop_tracker.check(tool_name, args_hash)
                    {
                        // Skip execution entirely — the model is stuck.
                        let skip_msg = loop_intervention_message(
                            tool_name,
                            "",
                            &LoopIntervention::Skip { count },
                        )
                        .unwrap_or_default();
                        transcript_events.push(transcript_event(
                            "tool_execution",
                            "tool",
                            "internal",
                            &skip_msg,
                            Some(serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "loop_skipped": true,
                                "repeat_count": count,
                            })),
                        ));
                        if tool_format == "native" {
                            append_message_to_contexts(
                                &mut visible_messages,
                                &mut recorded_messages,
                                build_tool_result_message(tool_id, &skip_msg, &opts.provider),
                            );
                        } else {
                            observations.push_str(&format!(
                                "[result of {tool_name}]\n{skip_msg}\n[end of {tool_name} result]\n\n"
                            ));
                        }
                        crate::tracing::span_end(tool_span_id);
                        if let Some(bridge) = bridge.as_ref() {
                            bridge.send_call_end(
                                &tool_call_id,
                                "tool",
                                tool_name,
                                0,
                                "loop_skipped",
                                serde_json::json!({"loop_skipped": true, "repeat_count": count}),
                            );
                        }
                        continue;
                    }
                }

                // Tool replay: if replay mode is active, try to use a
                // recorded fixture instead of executing the tool.
                let replay_hit = if crate::llm::mock::get_tool_recording_mode()
                    == crate::llm::mock::ToolRecordingMode::Replay
                {
                    crate::llm::mock::find_tool_replay_fixture(tool_name, &tool_args)
                } else {
                    None
                };

                let tool_start = std::time::Instant::now();
                let (is_rejected, result_text) = if let Some(fixture) = replay_hit {
                    (fixture.is_rejected, fixture.result.clone())
                } else {
                    // Prefer a pre-computed result from the parallel pre-fetch
                    // pass above, when available.
                    let call_result = if let Some(cached) = parallel_results.remove(&tc_index) {
                        cached
                    } else {
                        dispatch_tool_execution(
                            tool_name,
                            &tool_args,
                            tools_val,
                            bridge.as_ref(),
                            tool_retries,
                            tool_backoff_ms,
                        )
                        .await
                    };

                    let rejected =
                        matches!(
                            &call_result,
                            Err(VmError::CategorizedError {
                                category: ErrorCategory::ToolRejected,
                                ..
                            })
                        ) || call_result.as_ref().ok().is_some_and(is_denied_tool_result);
                    let text = match &call_result {
                        Ok(val) => render_tool_result(val),
                        Err(VmError::CategorizedError {
                            message,
                            category: ErrorCategory::ToolRejected,
                        }) => render_tool_result(&denied_tool_result(
                            tool_name,
                            format!("{message} Do not retry this tool."),
                        )),
                        Err(error) => format!("Error: {error}"),
                    };
                    (rejected, text)
                };

                if is_rejected && !rejected_tools.contains(&tool_name.to_string()) {
                    rejected_tools.push(tool_name.to_string());
                }

                // Track run() exit codes for verification-gated exit.
                // The host bridge formats run results with "exit_code=N" or
                // "Command succeeded"/"Command failed" markers.
                if exit_when_verified && tool_name == "run" {
                    if result_text.contains("exit_code=0")
                        || result_text.contains("Command succeeded")
                        || result_text.contains("success=true")
                    {
                        last_run_exit_code = Some(0);
                    } else if result_text.contains("Command failed")
                        || result_text.contains("success=false")
                        || result_text.contains("exit_code=")
                    {
                        last_run_exit_code = Some(1);
                    }
                }

                // Microcompaction: compress oversized tool outputs
                let result_text = if let Some(ref ac) = auto_compact {
                    if result_text.len() > ac.tool_output_max_chars {
                        if let Some(ref cb) = ac.compress_callback {
                            crate::orchestration::invoke_compress_callback(
                                cb,
                                tool_name,
                                &result_text,
                                ac.tool_output_max_chars,
                            )
                            .await
                        } else {
                            crate::orchestration::microcompact_tool_output(
                                &result_text,
                                ac.tool_output_max_chars,
                            )
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "status",
                    serde_json::json!(if is_rejected { "rejected" } else { "ok" }),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "result_chars",
                    serde_json::json!(result_text.len()),
                );

                // PostToolUse hooks (in-process)
                let result_text =
                    crate::orchestration::run_post_tool_hooks(tool_name, &result_text);

                // on_tool_result closure hook (Harn-level post-execution)
                let result_text = if let Some(VmValue::Closure(ref closure)) = config.on_tool_result
                {
                    if let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() {
                        let hook_arg = crate::stdlib::json_to_vm_value(&serde_json::json!({
                            "tool_name": tool_name,
                            "result": result_text,
                        }));
                        match vm.call_closure_pub(closure, &[hook_arg], &[]).await {
                            Ok(VmValue::String(s)) if !s.is_empty() => s.to_string(),
                            _ => result_text,
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };

                // Bridge-level PostToolUse gate: host can inspect/modify result
                let result_text = if let Some(bridge) = bridge.as_ref() {
                    let mutation = crate::orchestration::current_mutation_session();
                    if let Ok(response) = bridge
                        .call(
                            "tool/post_use",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "result": result_text,
                                "rejected": is_rejected,
                                "mutation": {
                                    "classification": classify_tool_mutation(tool_name),
                                    "session": mutation,
                                    "declared_paths": declared_paths(tool_name, &tool_args),
                                },
                            }),
                        )
                        .await
                    {
                        response
                            .get("result")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or(result_text)
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                if let Some(bridge) = bridge.as_ref() {
                    bridge.send_call_end(
                        &tool_call_id,
                        "tool",
                        tool_name,
                        tool_started_at.elapsed().as_millis() as u64,
                        if is_rejected { "rejected" } else { "ok" },
                        serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "iteration": iteration,
                            "classification": classify_tool_mutation(tool_name),
                            "declared_paths": declared_paths(tool_name, &tool_args),
                            "result_chars": result_text.len(),
                            "rejected": is_rejected,
                        }),
                    );
                }
                crate::tracing::span_end(tool_span_id);

                // Record tool call result if recording mode is active.
                if crate::llm::mock::get_tool_recording_mode()
                    == crate::llm::mock::ToolRecordingMode::Record
                {
                    crate::llm::mock::record_tool_call(crate::orchestration::ToolCallRecord {
                        tool_name: tool_name.to_string(),
                        tool_use_id: tool_call_id.clone(),
                        args_hash: crate::orchestration::tool_fixture_hash(tool_name, &tool_args),
                        result: result_text.clone(),
                        is_rejected,
                        duration_ms: tool_started_at.elapsed().as_millis() as u64,
                        iteration,
                        timestamp: crate::orchestration::now_rfc3339(),
                    });
                }

                // Tool loop detection: record the result and check for
                // repeated identical outcomes.  If we detect a loop,
                // append a redirection hint or replace the result.
                let result_text = if loop_detect_enabled && !is_rejected {
                    let result_hash = stable_hash_str(&result_text);
                    let intervention = loop_tracker.record(tool_name, args_hash, result_hash);
                    if let Some(msg) =
                        loop_intervention_message(tool_name, &result_text, &intervention)
                    {
                        let (kind, count) = match &intervention {
                            LoopIntervention::Warn { count } => ("warn", *count),
                            LoopIntervention::Block { count } => ("block", *count),
                            LoopIntervention::Skip { count } => ("skip", *count),
                            LoopIntervention::Proceed => ("proceed", 0),
                        };
                        super::trace::emit_agent_event(
                            super::trace::AgentTraceEvent::LoopIntervention {
                                tool_name: tool_name.to_string(),
                                kind: kind.to_string(),
                                count,
                                iteration,
                            },
                        );
                        match intervention {
                            LoopIntervention::Warn { .. } => {
                                // Append hint after the real result
                                format!("{result_text}{msg}")
                            }
                            LoopIntervention::Block { .. } => {
                                // Replace the result entirely with the redirect
                                msg
                            }
                            _ => result_text,
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                let tool_status = if is_rejected {
                    "rejected"
                } else if result_text.starts_with("Error:") || result_text.starts_with("ERROR:") {
                    "error"
                } else {
                    "ok"
                };
                tool_results_this_iter.push(serde_json::json!({
                    "tool_name": tool_name,
                    "status": tool_status,
                    "rejected": is_rejected,
                }));

                transcript_events.push(transcript_event(
                    "tool_execution",
                    "tool",
                    "internal",
                    &result_text,
                    Some(serde_json::json!({
                        "tool_name": tool_name,
                        "tool_use_id": tool_id,
                        "rejected": is_rejected,
                    })),
                ));

                if is_rejected {
                    super::trace::emit_agent_event(super::trace::AgentTraceEvent::ToolRejected {
                        tool_name: tool_name.to_string(),
                        reason: result_text.clone(),
                        iteration,
                    });
                } else {
                    super::trace::emit_agent_event(super::trace::AgentTraceEvent::ToolExecution {
                        tool_name: tool_name.to_string(),
                        tool_use_id: tool_id.to_string(),
                        duration_ms: tool_start.elapsed().as_millis() as u64,
                        status: tool_status.to_string(),
                        classification: classify_tool_mutation(tool_name),
                        iteration,
                    });
                }

                if tool_format == "native" {
                    append_message_to_contexts(
                        &mut visible_messages,
                        &mut recorded_messages,
                        build_tool_result_message(tool_id, &result_text, &opts.provider),
                    );
                } else {
                    observations.push_str(&format!(
                        "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                    ));
                }
            }

            all_tools_used.extend(tools_used_this_iter);
            if tool_format != "native" && !observations.is_empty() {
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "user",
                        "content": observations.trim_end(),
                    }),
                );
            }
            if !rejection_followups.is_empty() {
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "user",
                        "content": rejection_followups.join("\n\n"),
                    }),
                );
            }
            let finish_step_messages = inject_queued_user_messages(
                bridge.as_ref(),
                &mut visible_messages,
                crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
            )
            .await?;
            append_host_messages_to_recorded(&mut recorded_messages, &finish_step_messages);
            for message in &finish_step_messages {
                transcript_events.push(transcript_event(
                    "host_input",
                    "user",
                    "public",
                    &message.content,
                    Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                ));
            }
            if !finish_step_messages.is_empty() {
                consecutive_text_only = 0;
            }

            // Post-turn callback: let the pipeline inspect each tool turn
            // and optionally inject a user message (e.g. batching hints,
            // progress tracking, adaptive instructions).
            if tool_calls.len() == 1 {
                consecutive_single_tool_turns += 1;
            } else {
                consecutive_single_tool_turns = 0;
            }
            let successful_tool_names: Vec<&str> = tool_results_this_iter
                .iter()
                .filter(|result| result["status"].as_str() == Some("ok"))
                .filter_map(|result| result["tool_name"].as_str())
                .collect();
            for tool_name in &successful_tool_names {
                if !successful_tools_used
                    .iter()
                    .any(|existing| existing == tool_name)
                {
                    successful_tools_used.push((*tool_name).to_string());
                }
            }
            if let Some(VmValue::Closure(ref closure)) = config.post_turn_callback {
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .filter_map(|tc| tc["name"].as_str())
                    .collect();
                let turn_info = serde_json::json!({
                    "tool_names": tool_names,
                    "tool_results": tool_results_this_iter,
                    "successful_tool_names": successful_tool_names,
                    "tool_count": tool_calls.len(),
                    "iteration": iteration,
                    "consecutive_single_tool_turns": consecutive_single_tool_turns,
                    "session_tools_used": all_tools_used,
                    "session_successful_tools": successful_tools_used,
                });
                let cb_arg = crate::stdlib::json_to_vm_value(&turn_info);
                if let Ok(mut vm) = crate::vm::clone_async_builtin_child_vm()
                    .ok_or_else(|| VmError::Runtime("no VM context".into()))
                {
                    if let Ok(val) = vm.call_closure_pub(closure, &[cb_arg], &[]).await {
                        let directive = parse_post_turn_directive(&val);
                        if let Some(msg) = directive.message {
                            crate::events::log_debug(
                                "agent.post_turn",
                                &format!("iter={iteration} injecting nudge ({} chars)", msg.len()),
                            );
                            append_message_to_contexts(
                                &mut visible_messages,
                                &mut recorded_messages,
                                serde_json::json!({
                                    "role": "user",
                                    "content": msg,
                                }),
                            );
                            consecutive_single_tool_turns = 0;
                        }
                        if directive.stop {
                            crate::events::log_debug(
                                "agent.post_turn",
                                &format!("iter={iteration} requested stage stop"),
                            );
                            break;
                        }
                    }
                }
            }
            if let Some(stop_tools) = config.stop_after_successful_tools.as_ref() {
                if should_stop_after_successful_tools(&tool_results_this_iter, stop_tools) {
                    crate::events::log_debug(
                        "agent.stop_after_successful_tools",
                        &format!(
                            "iter={iteration} requested stage stop after successful tool turn"
                        ),
                    );
                    break;
                }
            }

            // Auto-compaction check after tool processing.
            // Include the system prompt + tool definitions in the estimate
            // since they consume context window alongside messages.
            if let Some(ref ac) = auto_compact {
                let mut est = crate::orchestration::estimate_message_tokens(&visible_messages);
                if let Some(ref sys) = opts.system {
                    est += sys.len() / 4;
                }
                if est > ac.token_threshold {
                    let mut compact_opts = opts.clone();
                    compact_opts.messages = visible_messages.clone();
                    if let Some(summary) = crate::orchestration::auto_compact_messages(
                        &mut visible_messages,
                        ac,
                        Some(&compact_opts),
                    )
                    .await?
                    {
                        super::trace::emit_agent_event(
                            super::trace::AgentTraceEvent::ContextCompaction {
                                archived_messages: est.saturating_sub(
                                    crate::orchestration::estimate_message_tokens(
                                        &visible_messages,
                                    ),
                                ),
                                new_summary_len: summary.len(),
                                iteration,
                            },
                        );
                        let merged = match transcript_summary.take() {
                            Some(existing)
                                if !existing.trim().is_empty()
                                    && existing.trim() != summary.trim() =>
                            {
                                format!("{existing}\n\n{summary}")
                            }
                            Some(_) | None => summary,
                        };
                        transcript_summary = Some(merged);
                    }
                }
            }

            // Feed parse-error diagnostics back in the mixed case too, so the
            // model can correct its syntax in the next turn (mirrors the
            // text-only branch below). Without this, rejected calls would
            // silently disappear from the conversation.
            if !tool_parse_errors.is_empty() {
                let error_msg = tool_parse_errors.join("\n\n");
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "user",
                        "content": error_msg,
                    }),
                );
            }
            if sentinel_hit {
                if !tool_parse_errors.is_empty() {
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "{} tool-call parse error(s) suppressed by sentinel: {}",
                            tool_parse_errors.len(),
                            tool_parse_errors.join("; ")
                        ),
                    );
                }
                break;
            }
            continue;
        }

        // If the model attempted tool calls but parsing failed, replace the
        // raw malformed assistant text with a compact placeholder before
        // replaying history. Otherwise the next iteration sees its own
        // broken syntax and mutates it further (observed self-poison loop).
        //
        // In the clean text-only case we still use `text_prose` (not `text`)
        // so that IF the model wrote prose AND a non-malformed call that
        // simply happened to have no downstream effect (e.g. a call whose
        // tool was later filtered), history carries only the prose — not
        // the call expression as accidental narration.
        let assistant_content_for_history = if !tool_parse_errors.is_empty() {
            compact_malformed_assistant_turn(tool_parse_errors.len())
        } else {
            shaped_text_prose.clone()
        };
        append_message_to_contexts(
            &mut visible_messages,
            &mut recorded_messages,
            serde_json::json!({
                "role": "assistant",
                "content": assistant_content_for_history,
            }),
        );

        // Sentinel check for text-only responses (no tool calls).
        if sentinel_hit {
            break;
        }

        // If the model attempted tool calls but parsing failed, send diagnostics
        // back so it can fix its syntax instead of being silently nudged.
        if !tool_parse_errors.is_empty() {
            let error_msg = tool_parse_errors.join("\n\n");
            append_message_to_contexts(
                &mut visible_messages,
                &mut recorded_messages,
                serde_json::json!({
                    "role": "user",
                    "content": error_msg,
                }),
            );
            tool_parse_errors.clear();
            consecutive_text_only = 0;
            continue;
        }

        // done_sentinel already checked before tool dispatch above;
        // this path only reached for text-only responses without sentinel.
        if !persistent && !daemon {
            break;
        }

        // Daemon mode: if no tool calls and agent is idle, notify host and
        // wait briefly for user messages before deciding to continue/exit.
        if daemon && !persistent {
            daemon_state = "idle".to_string();
            if daemon_config.consolidate_on_idle {
                maybe_auto_compact_agent_messages(
                    opts,
                    &auto_compact,
                    &mut visible_messages,
                    &mut transcript_summary,
                )
                .await?;
            }
            let idle_snapshot = daemon_snapshot_from_state(
                &daemon_state,
                &visible_messages,
                &recorded_messages,
                &transcript_summary,
                &transcript_events,
                &total_text,
                &last_iteration_text,
                &all_tools_used,
                &rejected_tools,
                &deferred_user_messages,
                total_iterations,
                idle_backoff_ms,
                last_run_exit_code,
                &daemon_watch_state,
            );
            daemon_snapshot_path = maybe_persist_daemon_snapshot(&daemon_config, &idle_snapshot)?
                .or(daemon_snapshot_path);
            if !daemon_config.has_wake_source(bridge.is_some()) {
                final_status = "idle";
                break;
            }
            loop {
                if let Some(bridge) = bridge.as_ref() {
                    bridge.notify(
                        "agent/idle",
                        serde_json::json!({
                            "iteration": total_iterations,
                            "backoff_ms": idle_backoff_ms,
                            "persist_path": daemon_snapshot_path,
                            "watch_paths": daemon_config.watch_paths,
                        }),
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(
                    daemon_config.idle_wait_ms(idle_backoff_ms),
                ))
                .await;
                let resumed = bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.take_resume_signal());
                let idle_messages = inject_queued_user_messages(
                    bridge.as_ref(),
                    &mut visible_messages,
                    crate::bridge::DeliveryCheckpoint::InterruptImmediate,
                )
                .await?;
                append_host_messages_to_recorded(&mut recorded_messages, &idle_messages);
                let changed_paths = if daemon_config.watch_paths.is_empty() {
                    Vec::new()
                } else {
                    detect_watch_changes(&daemon_config.watch_paths, &mut daemon_watch_state)
                };
                for message in &idle_messages {
                    transcript_events.push(transcript_event(
                        "host_input",
                        "user",
                        "public",
                        &message.content,
                        Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                    ));
                }
                let wake_reason = if !idle_messages.is_empty() {
                    Some(("message", None))
                } else if resumed {
                    Some(("resume", None))
                } else if !changed_paths.is_empty() {
                    Some((
                        "watch",
                        Some(format!(
                            "Daemon wake: watched paths changed: {}. Re-check the task state and act only if something actually changed.",
                            changed_paths.join(", ")
                        )),
                    ))
                } else if daemon_config.wake_interval_ms.is_some() {
                    Some((
                        "timer",
                        Some(
                            "Daemon timer wake fired. Re-check for background work and only act when there is new information or a pending follow-up."
                                .to_string(),
                        ),
                    ))
                } else {
                    None
                };
                if let Some((reason, wake_message)) = wake_reason {
                    if let Some(message) = wake_message {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            serde_json::json!({
                                "role": "user",
                                "content": message,
                            }),
                        );
                    }
                    transcript_events.push(transcript_event(
                        "daemon_wake",
                        "system",
                        "internal",
                        reason,
                        Some(serde_json::json!({
                            "reason": reason,
                            "watch_paths": changed_paths,
                            "resumed": resumed,
                        })),
                    ));
                    daemon_state = "active".to_string();
                    consecutive_text_only = 0;
                    idle_backoff_ms = 100;
                    break;
                }
                daemon_config.update_idle_backoff(&mut idle_backoff_ms);
            }
            continue;
        }

        let finish_step_messages = inject_queued_user_messages(
            bridge.as_ref(),
            &mut visible_messages,
            crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
        )
        .await?;
        append_host_messages_to_recorded(&mut recorded_messages, &finish_step_messages);
        for message in &finish_step_messages {
            transcript_events.push(transcript_event(
                "host_input",
                "user",
                "public",
                &message.content,
                Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
            ));
        }
        if !finish_step_messages.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
            continue;
        }

        consecutive_text_only += 1;
        if consecutive_text_only > max_nudges {
            final_status = "stuck";
            break;
        }

        // Silent continuation for short prose: when the model emits a
        // short text-only response (< 150 tokens, typically "thinking"
        // statements like "Let me check..."), don't inject a nudge. Just
        // loop back — the model sees its own text as the last assistant
        // message and naturally continues to act. This avoids polluting
        // context with nudge messages and the "nudge → rephrase → nudge"
        // loop seen with chatty models.
        //
        let nudge = action_turn_nudge(&tool_format, config.turn_policy.as_ref(), prose_too_long)
            .or_else(|| custom_nudge.clone())
            .unwrap_or_else(|| "Continue — use a tool call to make progress.".to_string());
        append_message_to_contexts(
            &mut visible_messages,
            &mut recorded_messages,
            serde_json::json!({
                "role": "user",
                "content": nudge,
            }),
        );
    }

    deferred_user_messages.extend(
        inject_queued_user_messages(
            bridge.as_ref(),
            &mut visible_messages,
            crate::bridge::DeliveryCheckpoint::EndOfInteraction,
        )
        .await?
        .into_iter()
        .map(|message| message.content),
    );

    if daemon && final_status == "done" {
        final_status = "idle";
    }
    if final_status == "done" {
        if let Some(required_tools) = config.require_successful_tools.as_ref() {
            if !required_tools.is_empty()
                && !successful_tools_used
                    .iter()
                    .any(|tool_name| required_tools.iter().any(|wanted| wanted == tool_name))
            {
                final_status = "failed";
            }
        }
    }
    if daemon {
        daemon_state = final_status.to_string();
        let final_snapshot = daemon_snapshot_from_state(
            &daemon_state,
            &visible_messages,
            &recorded_messages,
            &transcript_summary,
            &transcript_events,
            &total_text,
            &last_iteration_text,
            &all_tools_used,
            &rejected_tools,
            &deferred_user_messages,
            total_iterations,
            idle_backoff_ms,
            last_run_exit_code,
            &daemon_watch_state,
        );
        daemon_snapshot_path = maybe_persist_daemon_snapshot(&daemon_config, &final_snapshot)?
            .or(daemon_snapshot_path);
    }

    // Emit structured trace event for loop completion.
    super::trace::emit_agent_event(super::trace::AgentTraceEvent::LoopComplete {
        status: final_status.to_string(),
        iterations: total_iterations,
        total_duration_ms: loop_start.elapsed().as_millis() as u64,
        tools_used: all_tools_used.clone(),
        successful_tools: successful_tools_used.clone(),
    });
    let trace_summary = super::trace::agent_trace_summary();

    Ok(serde_json::json!({
        "status": final_status,
        "daemon_state": daemon_state,
        "daemon_snapshot_path": daemon_snapshot_path,
        "text": total_text,
        "visible_text": last_iteration_text,
        "iterations": total_iterations,
        "duration_ms": loop_start.elapsed().as_millis() as i64,
        "tools_used": all_tools_used,
        "successful_tools": successful_tools_used,
        "rejected_tools": rejected_tools,
        "tool_calling_mode": tool_format,
        "deferred_user_messages": deferred_user_messages,
        "trace": trace_summary,
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            opts.transcript_id.clone(),
            transcript_summary,
            opts.transcript_metadata.clone(),
            &recorded_messages,
            transcript_events,
            Vec::new(),
            Some(if final_status == "done" { "active" } else { "paused" }),
        )),
    }))
}

/// Register a tool-aware `agent_loop` that uses a bridge for tool execution.
/// This overrides the native text-only agent_loop with one that can:
/// 1. Pass tool definitions to the LLM
/// 2. Execute tool calls via the bridge (delegated to host)
/// 3. Feed tool results back into the conversation
#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
