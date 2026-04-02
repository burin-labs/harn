use std::rc::Rc;

use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::helpers::{
    extract_llm_options, opt_bool, opt_int, opt_str, transcript_event, transcript_to_vm_with_events,
};
use super::tools::{
    build_assistant_response_message, build_assistant_tool_message,
    build_tool_calling_contract_prompt, build_tool_result_message, handle_tool_locally,
    normalize_tool_args, parse_text_tool_calls,
};
use super::trace::{trace_llm_call, LlmTraceEntry};

fn next_call_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

thread_local! {
    static CURRENT_HOST_BRIDGE: std::cell::RefCell<Option<Rc<crate::bridge::HostBridge>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub struct AgentLoopConfig {
    pub persistent: bool,
    pub max_iterations: usize,
    pub max_nudges: usize,
    pub nudge: Option<String>,
    pub done_sentinel: Option<String>,
    pub tool_retries: usize,
    pub tool_backoff_ms: u64,
    pub tool_format: String,
    /// Auto-compaction config. When set, the agent loop automatically compacts
    /// the transcript when estimated tokens exceed the threshold.
    pub auto_compact: Option<crate::orchestration::AutoCompactConfig>,
    /// Capability policy scoped to this agent loop.
    pub policy: Option<crate::orchestration::CapabilityPolicy>,
    /// Daemon mode: agent stays alive waiting for user messages instead of
    /// terminating after the LLM produces text without tool calls.
    pub daemon: bool,
    /// LLM call retry count for transient errors (429, 5xx, connection).
    pub llm_retries: usize,
    /// Base backoff in milliseconds between LLM retries.
    pub llm_backoff_ms: u64,
}

/// Classify whether a VmError from an LLM call is transient and worth retrying.
fn is_retryable_llm_error(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(VmValue::String(s)) => s.to_lowercase(),
        VmError::CategorizedError { category, .. } => {
            return matches!(
                category,
                crate::value::ErrorCategory::RateLimit | crate::value::ErrorCategory::Timeout
            );
        }
        VmError::Runtime(s) => s.to_lowercase(),
        _ => return false,
    };
    // Retryable HTTP status codes
    msg.contains("http 429")
        || msg.contains("http 500")
        || msg.contains("http 502")
        || msg.contains("http 503")
        || msg.contains("http 529")
        || msg.contains("overloaded")
        || msg.contains("rate limit")
        || msg.contains("too many requests")
        // Connection/timeout errors
        || msg.contains("stream error")
        || msg.contains("connection")
        || msg.contains("timed out")
        || msg.contains("timeout")
}

/// Extract retry-after delay from error message if present (e.g. "retry-after: 5").
fn extract_retry_after_ms(err: &VmError) -> Option<u64> {
    let msg = match err {
        VmError::Thrown(VmValue::String(s)) => s.as_ref(),
        VmError::Runtime(s) => s.as_str(),
        _ => return None,
    };
    let lower = msg.to_lowercase();
    if let Some(pos) = lower.find("retry-after:") {
        let after = &msg[pos + "retry-after:".len()..];
        let trimmed = after.trim_start();
        if let Some(num_str) = trimmed.split_whitespace().next() {
            if let Ok(secs) = num_str.parse::<f64>() as Result<f64, _> {
                return Some((secs * 1000.0) as u64);
            }
        }
    }
    None
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

fn classify_tool_mutation(tool_name: &str) -> &'static str {
    match tool_name {
        "write_file" | "edit" | "create_file" | "apply_patch" | "insert_function"
        | "replace_body" | "add_import" => "apply_workspace",
        "delete_file" | "remove_file" | "move_file" | "rename_file" => "destructive",
        "exec" | "shell" | "exec_at" | "shell_at" | "run" => "ambient_side_effect",
        _ if tool_name.starts_with("mcp_") => "host_defined",
        _ => "read_only",
    }
}

fn declared_paths(tool_args: &serde_json::Value) -> Vec<String> {
    let Some(map) = tool_args.as_object() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for key in ["path", "file", "cwd", "repo", "target", "destination"] {
        if let Some(value) = map.get(key).and_then(|value| value.as_str()) {
            if !value.is_empty() {
                paths.push(value.to_string());
            }
        }
    }
    if let Some(items) = map.get("paths").and_then(|value| value.as_array()) {
        for item in items {
            if let Some(value) = item.as_str() {
                if !value.is_empty() {
                    paths.push(value.to_string());
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

async fn inject_queued_user_messages(
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    opts: &mut super::api::LlmCallOptions,
    checkpoint: crate::bridge::DeliveryCheckpoint,
) -> Result<Vec<crate::bridge::QueuedUserMessage>, VmError> {
    let Some(bridge) = bridge else {
        return Ok(Vec::new());
    };
    let queued = bridge.take_queued_user_messages_for(checkpoint).await;
    for message in &queued {
        opts.messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
    Ok(queued)
}

pub(crate) fn agent_loop_result_from_llm(
    result: &super::api::LlmResult,
    opts: super::api::LlmCallOptions,
) -> serde_json::Value {
    let mut transcript_messages = opts.messages.clone();
    transcript_messages.push(build_assistant_response_message(
        &result.text,
        &result.blocks,
        &result.tool_calls,
        &opts.provider,
    ));
    let mut events = vec![transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
        })),
    )];
    if let Some(thinking) = result.thinking.clone() {
        if !thinking.is_empty() {
            events.push(transcript_event(
                "private_reasoning",
                "assistant",
                "private",
                &thinking,
                None,
            ));
        }
    }
    serde_json::json!({
        "status": "done",
        "text": result.text,
        "visible_text": result.text,
        "private_reasoning": result.thinking,
        "iterations": 1,
        "duration_ms": 0,
        "tools_used": [],
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            opts.transcript_id,
            opts.transcript_summary,
            opts.transcript_metadata,
            &transcript_messages,
            events,
            Vec::new(),
            Some("active"),
        )),
    })
}

/// Create an unbounded channel and spawn a local task that forwards text
/// deltas to `bridge.send_call_progress()`.  Returns the sender half —
/// drop it when the LLM call is done to terminate the forwarding task.
fn spawn_progress_forwarder(
    bridge: &Rc<crate::bridge::HostBridge>,
    call_id: String,
) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let bridge = bridge.clone();
    tokio::task::spawn_local(async move {
        let mut token_count: u64 = 0;
        while let Some(delta) = rx.recv().await {
            token_count += 1;
            bridge.send_call_progress(&call_id, &delta, token_count);
        }
    });
    tx
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

    let bridge = current_host_bridge();
    let max_iterations = config.max_iterations;
    let persistent = config.persistent;
    let max_nudges = config.max_nudges;
    let custom_nudge = config.nudge;
    let done_sentinel = config
        .done_sentinel
        .clone()
        .unwrap_or_else(|| "##DONE##".to_string());
    let tool_retries = config.tool_retries;
    let tool_backoff_ms = config.tool_backoff_ms;
    let tool_format = config.tool_format;

    let auto_compact = config.auto_compact.clone();
    let daemon = config.daemon;

    // Push per-agent policy if configured
    if let Some(ref policy) = config.policy {
        crate::orchestration::push_execution_policy(policy.clone());
    }
    let _policy_guard = ExecutionPolicyGuard {
        active: config.policy.is_some(),
    };

    let tools_val = opts.tools.as_ref();
    let has_tools = !opts
        .native_tools
        .as_ref()
        .map(|v| v.is_empty())
        .unwrap_or(true)
        || tools_val.is_some();

    if has_tools && tool_format != "native" {
        opts.native_tools = None;
    }
    if has_tools {
        let system_prompt = opts.system.get_or_insert_with(String::new);
        system_prompt.push_str(&build_tool_calling_contract_prompt(
            tools_val,
            &tool_format,
            tool_format == "text",
        ));
    }

    if persistent {
        let system_prompt = opts.system.get_or_insert_with(String::new);
        system_prompt.push_str(&format!(
            "\n\nIMPORTANT: You MUST keep working until the task is complete. \
             Do NOT stop to explain or summarize — take action with tools. \
             When the requested work is complete and your verification has succeeded, \
             stop immediately and output {done_sentinel} on its own line. \
             Do not make additional tool calls after a passing verification result unless \
             you still have concrete evidence that the task is incomplete or failing."
        ));
    }

    let mut total_text = String::new();
    let mut consecutive_text_only = 0usize;
    let mut all_tools_used: Vec<String> = Vec::new();
    let mut rejected_tools: Vec<String> = Vec::new();
    let mut deferred_user_messages: Vec<String> = Vec::new();
    let mut total_iterations = 0usize;
    let mut final_status = "done";
    let loop_start = std::time::Instant::now();
    let mut transcript_events = Vec::new();
    let mut idle_backoff_ms = 100u64;

    for iteration in 0..max_iterations {
        total_iterations = iteration + 1;
        let immediate_messages = inject_queued_user_messages(
            bridge.as_ref(),
            opts,
            crate::bridge::DeliveryCheckpoint::InterruptImmediate,
        )
        .await?;
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
        let start = std::time::Instant::now();
        let result = if let Some(bridge) = bridge.as_ref() {
            let mut llm_attempt = 0usize;
            loop {
                let llm_call_id = next_call_id();
                let prompt_chars: usize = opts
                    .messages
                    .iter()
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .map(|s| s.len())
                    .sum();
                bridge.send_call_start(
                    &llm_call_id,
                    "llm",
                    "llm_call",
                    serde_json::json!({
                        "model": opts.model,
                        "prompt_chars": prompt_chars,
                        "iteration": iteration,
                        "llm_attempt": llm_attempt,
                    }),
                );
                let delta_tx = spawn_progress_forwarder(bridge, llm_call_id.clone());
                let llm_result = vm_call_llm_full_streaming(opts, delta_tx).await;
                let llm_duration = start.elapsed().as_millis() as u64;
                match llm_result {
                    Ok(result) => {
                        bridge.send_call_end(
                            &llm_call_id,
                            "llm",
                            "llm_call",
                            llm_duration,
                            "ok",
                            serde_json::json!({
                                "model": result.model,
                                "input_tokens": result.input_tokens,
                                "output_tokens": result.output_tokens,
                            }),
                        );
                        break result;
                    }
                    Err(error) => {
                        let retryable = is_retryable_llm_error(&error);
                        let can_retry = retryable && llm_attempt < config.llm_retries;
                        let status = if can_retry { "retrying" } else if retryable { "retries_exhausted" } else { "error" };
                        bridge.send_call_end(
                            &llm_call_id,
                            "llm",
                            "llm_call",
                            llm_duration,
                            status,
                            serde_json::json!({
                                "error": error.to_string(),
                                "retryable": retryable,
                                "attempt": llm_attempt,
                            }),
                        );
                        if !can_retry {
                            return Err(error);
                        }
                        llm_attempt += 1;
                        let backoff = extract_retry_after_ms(&error)
                            .unwrap_or(config.llm_backoff_ms * (1 << llm_attempt.min(4)) as u64);
                        eprintln!(
                            "[harn] LLM call failed ({}), retrying in {}ms (attempt {}/{})",
                            error, backoff, llm_attempt, config.llm_retries
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                    }
                }
            }
        } else {
            super::api::vm_call_llm_full(opts).await?
        };

        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });

        let text = result.text.clone();
        total_text.push_str(&text);
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

        let mut tool_call_source = "none";
        let tool_calls = if !result.tool_calls.is_empty() {
            tool_call_source = "native";
            result.tool_calls.clone()
        } else if has_tools {
            // Prefer provider-native tool calls when available, but keep text-call
            // parsing as a compatibility fallback. This lets workflows use
            // tool_format="native" without breaking providers or models that still
            // emit ```call blocks.
            let parsed = parse_text_tool_calls(&text);
            if !parsed.is_empty() {
                tool_call_source = "text_fallback";
                if tool_format == "native" {
                    eprintln!(
                        "[harn] text_fallback_triggered: model emitted {} text call(s) in native mode",
                        parsed.len()
                    );
                }
            }
            parsed
        } else {
            Vec::new()
        };
        if std::env::var("BURIN_TRACE_HARN_TOOL_PARSE").as_deref() == Ok("1") {
            eprintln!(
                "[harn-vm] tool_call_source={} count={} text_len={}",
                tool_call_source,
                tool_calls.len(),
                text.len()
            );
        }

        if !tool_calls.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
            if tool_format == "native" {
                opts.messages.push(build_assistant_tool_message(
                    &text,
                    &tool_calls,
                    &opts.provider,
                ));
            } else {
                opts.messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": text,
                }));
            }

            let mut observations = String::new();
            let mut tools_used_this_iter = Vec::new();
            let mut rejection_followups: Vec<String> = Vec::new();
            for tc in &tool_calls {
                let tool_id = tc["id"].as_str().unwrap_or("");
                let tool_name = tc["name"].as_str().unwrap_or("");
                let mut tool_args = normalize_tool_args(tool_name, &tc["arguments"]);
                if std::env::var("BURIN_TRACE_HARN_TOOL_PARSE").as_deref() == Ok("1") {
                    eprintln!(
                        "[harn-vm] parsed_tool_call name={} args={}",
                        tool_name, tc["arguments"]
                    );
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
                    let result_text = format!(
                        "REJECTED: {}. Use one of the declared tools exactly as named and put extra fields inside that tool's arguments.",
                        error
                    );
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
                        opts.messages.push(build_tool_result_message(
                            tool_id,
                            &result_text,
                            &opts.provider,
                        ));
                    } else {
                        observations.push_str(&format!(
                            "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
                        ));
                    }
                    continue;
                }

                // PreToolUse hooks: in-process hooks first, then bridge gate
                match crate::orchestration::run_pre_tool_hooks(tool_name, &tool_args) {
                    crate::orchestration::PreToolAction::Allow => {}
                    crate::orchestration::PreToolAction::Deny(reason) => {
                        let result_text = format!("REJECTED by hook: {reason}");
                        if !rejected_tools.contains(&tool_name.to_string()) {
                            rejected_tools.push(tool_name.to_string());
                        }
                        transcript_events.push(transcript_event(
                            "tool_execution", "tool", "internal", &result_text,
                            Some(serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true})),
                        ));
                        if tool_format == "native" {
                            opts.messages.push(build_tool_result_message(
                                tool_id,
                                &result_text,
                                &opts.provider,
                            ));
                        } else {
                            observations.push_str(&format!(
                                "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
                            ));
                        }
                        continue;
                    }
                    crate::orchestration::PreToolAction::Modify(new_args) => {
                        tool_args = new_args;
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
                                    "declared_paths": declared_paths(&tool_args),
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
                                let result_text = format!("REJECTED by host: {reason}");
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
                                    opts.messages.push(build_tool_result_message(
                                        tool_id,
                                        &result_text,
                                        &opts.provider,
                                    ));
                                } else {
                                    observations.push_str(&format!(
                                        "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
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

                let call_result = {
                    let mut attempt = 0usize;
                    loop {
                        let result = if let Some(local_result) =
                            handle_tool_locally(tool_name, &tool_args)
                        {
                            Ok(serde_json::Value::String(local_result))
                        } else if let Some(bridge) = bridge.as_ref() {
                            bridge
                                .call(
                                    "builtin_call",
                                    serde_json::json!({
                                        "name": tool_name,
                                        "args": [tool_args],
                                    }),
                                )
                                .await
                        } else {
                            Err(VmError::CategorizedError {
                                message: format!(
                                    "tool not available without host bridge: {tool_name}"
                                ),
                                category: ErrorCategory::ToolRejected,
                            })
                        };
                        match &result {
                            Ok(_) => break result,
                            Err(VmError::CategorizedError {
                                category: ErrorCategory::ToolRejected,
                                ..
                            }) => break result,
                            Err(_) if attempt < tool_retries => {
                                attempt += 1;
                                let delay = tool_backoff_ms * (1u64 << attempt.min(5));
                                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                            }
                            Err(_) => break result,
                        }
                    }
                };

                let is_rejected = matches!(
                    &call_result,
                    Err(VmError::CategorizedError {
                        category: ErrorCategory::ToolRejected,
                        ..
                    })
                );
                let result_text = match &call_result {
                    Ok(val) => {
                        if let Some(text) = val.as_str() {
                            text.to_string()
                        } else if val.is_null() {
                            "(no output)".to_string()
                        } else {
                            serde_json::to_string_pretty(val).unwrap_or_default()
                        }
                    }
                    Err(VmError::CategorizedError {
                        message,
                        category: ErrorCategory::ToolRejected,
                    }) => {
                        format!("REJECTED: {message} Do not retry this tool.")
                    }
                    Err(error) => format!("Error: {error}"),
                };

                if is_rejected && !rejected_tools.contains(&tool_name.to_string()) {
                    rejected_tools.push(tool_name.to_string());
                }

                // Microcompaction: snip oversized tool outputs
                let result_text = if let Some(ref ac) = auto_compact {
                    crate::orchestration::microcompact_tool_output(
                        &result_text,
                        ac.tool_output_max_chars,
                    )
                } else {
                    result_text
                };

                // PostToolUse hooks (in-process)
                let result_text =
                    crate::orchestration::run_post_tool_hooks(tool_name, &result_text);

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
                                    "declared_paths": declared_paths(&tool_args),
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

                if tool_format == "native" {
                    opts.messages.push(build_tool_result_message(
                        tool_id,
                        &result_text,
                        &opts.provider,
                    ));
                } else {
                    observations.push_str(&format!(
                        "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
                    ));
                }
            }

            all_tools_used.extend(tools_used_this_iter);
            if tool_format != "native" && !observations.is_empty() {
                opts.messages.push(serde_json::json!({
                    "role": "user",
                    "content": observations.trim_end(),
                }));
            }
            if !rejection_followups.is_empty() {
                opts.messages.push(serde_json::json!({
                    "role": "user",
                    "content": rejection_followups.join("\n\n"),
                }));
            }
            let finish_step_messages = inject_queued_user_messages(
                bridge.as_ref(),
                opts,
                crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
            )
            .await?;
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

            // Auto-compaction check after tool processing
            if let Some(ref ac) = auto_compact {
                let est = crate::orchestration::estimate_message_tokens(&opts.messages);
                if est > ac.token_threshold {
                    let compact_opts = opts.clone();
                    crate::orchestration::auto_compact_messages(
                        &mut opts.messages,
                        ac,
                        Some(&compact_opts),
                    )
                    .await?;
                }
            }

            continue;
        }

        opts.messages.push(serde_json::json!({
            "role": "assistant",
            "content": text,
        }));

        if persistent && text.contains(&done_sentinel) {
            break;
        }
        if !persistent && !daemon {
            break;
        }

        // Daemon mode: if no tool calls and agent is idle, notify host and
        // wait briefly for user messages before deciding to continue/exit.
        if daemon && !persistent {
            let Some(bridge) = bridge.as_ref() else {
                final_status = "idle";
                break;
            };
            loop {
                bridge.notify(
                    "agent/idle",
                    serde_json::json!({
                        "iteration": total_iterations,
                        "backoff_ms": idle_backoff_ms,
                    }),
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(idle_backoff_ms)).await;
                let resumed = bridge.take_resume_signal();
                let idle_messages = inject_queued_user_messages(
                    Some(bridge),
                    opts,
                    crate::bridge::DeliveryCheckpoint::InterruptImmediate,
                )
                .await?;
                if resumed || !idle_messages.is_empty() {
                    for message in &idle_messages {
                        transcript_events.push(transcript_event(
                            "host_input",
                            "user",
                            "public",
                            &message.content,
                            Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                        ));
                    }
                    consecutive_text_only = 0;
                    idle_backoff_ms = 100;
                    break;
                }
                idle_backoff_ms = match idle_backoff_ms {
                    0..=100 => 500,
                    101..=500 => 1000,
                    1001..=1999 => 2000,
                    _ => 2000,
                };
            }
            continue;
        }

        let finish_step_messages = inject_queued_user_messages(
            bridge.as_ref(),
            opts,
            crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
        )
        .await?;
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

        let nudge = custom_nudge.clone().unwrap_or_else(|| {
            if consecutive_text_only == 1 {
                if tool_format == "native" {
                    "You must use tools to complete this task. Start with the best available tool."
                        .to_string()
                } else {
                    "You must use tools to complete this task. Respond with a real ```call block, not a prose description of the tool you intend to use.\nExample:\n```call\nread(path=\"relative/path\")\n```"
                        .to_string()
                }
            } else if consecutive_text_only <= 3 {
                if tool_format == "native" {
                    "STOP explaining and USE TOOLS NOW. Include a concrete tool call."
                        .to_string()
                } else {
                    "STOP explaining and USE TOOLS NOW. A plain-English plan is a failure here. Reply with one or more actual ```call blocks only.\nExample:\n```call\nrun(command=\"<scoped verification command>\")\n```"
                        .to_string()
                }
            } else {
                "FINAL WARNING: call a tool now or the task will fail.".to_string()
            }
        });
        opts.messages.push(serde_json::json!({
            "role": "user",
            "content": nudge,
        }));
    }

    deferred_user_messages.extend(
        inject_queued_user_messages(
            bridge.as_ref(),
            opts,
            crate::bridge::DeliveryCheckpoint::EndOfInteraction,
        )
        .await?
        .into_iter()
        .map(|message| message.content),
    );

    Ok(serde_json::json!({
        "status": final_status,
        "text": total_text,
        "visible_text": total_text,
        "iterations": total_iterations,
        "duration_ms": loop_start.elapsed().as_millis() as i64,
        "tools_used": all_tools_used,
        "rejected_tools": rejected_tools,
        "tool_calling_mode": tool_format,
        "deferred_user_messages": deferred_user_messages,
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            opts.transcript_id.clone(),
            opts.transcript_summary.clone(),
            opts.transcript_metadata.clone(),
            &opts.messages,
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
pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    install_current_host_bridge(b.clone());
    vm.register_async_builtin("agent_loop", move |args| {
        let captured_bridge = b.clone();
        async move {
            std::mem::drop(captured_bridge);
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
            let persistent = opt_bool(&options, "persistent");
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
            let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
            let tool_format =
                opt_str(&options, "tool_format").unwrap_or_else(|| "text".to_string());
            let done_sentinel = opt_str(&options, "done_sentinel");
            let daemon = opt_bool(&options, "daemon");
            let auto_compact = if opt_bool(&options, "auto_compact") {
                let mut ac = crate::orchestration::AutoCompactConfig::default();
                if let Some(v) = opt_int(&options, "compact_threshold") {
                    ac.token_threshold = v as usize;
                }
                if let Some(v) = opt_int(&options, "tool_output_max_chars") {
                    ac.tool_output_max_chars = v as usize;
                }
                if let Some(v) = opt_int(&options, "compact_keep_last") {
                    ac.keep_last = v as usize;
                }
                if let Some(strategy) = opt_str(&options, "compact_strategy") {
                    ac.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
                    ac.custom_compactor = Some(callback.clone());
                    if !options
                        .as_ref()
                        .is_some_and(|o| o.contains_key("compact_strategy"))
                    {
                        ac.compact_strategy = crate::orchestration::CompactStrategy::Custom;
                    }
                }
                Some(ac)
            } else {
                None
            };
            // Parse per-agent policy from options dict
            let policy = options.as_ref().and_then(|o| o.get("policy")).map(|v| {
                let json = crate::llm::helpers::vm_value_to_json(v);
                serde_json::from_value::<crate::orchestration::CapabilityPolicy>(json)
                    .unwrap_or_default()
            });
            let mut opts = extract_llm_options(&args)?;
            let result = run_agent_loop_internal(
                &mut opts,
                AgentLoopConfig {
                    persistent,
                    max_iterations,
                    max_nudges,
                    nudge: custom_nudge,
                    done_sentinel,
                    tool_retries,
                    tool_backoff_ms,
                    tool_format,
                    auto_compact,
                    policy,
                    daemon,
                    llm_retries: opt_int(&options, "llm_retries").unwrap_or(2) as usize,
                    llm_backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
                },
            )
            .await?;
            Ok(crate::stdlib::json_to_vm_value(&result))
        }
    });
}

/// Register a bridge-aware `llm_call` that emits call_start/call_end notifications.
/// This overrides the native llm_call with one that reports to the host for observability.
pub fn register_llm_call_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    use super::api::vm_build_llm_result;
    use super::helpers::extract_json;
    use crate::stdlib::json_to_vm_value;

    let b = bridge;
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            eprintln!("[llm-debug] llm_call entered, extracting options");
            let opts = extract_llm_options(&args)?;
            eprintln!(
                "[llm-debug] Options extracted: provider={} model={}",
                opts.provider, opts.model
            );

            let call_id = next_call_id();
            let prompt_chars: usize = opts
                .messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .map(|s| s.len())
                .sum();
            bridge.send_call_start(
                &call_id,
                "llm",
                "llm_call",
                serde_json::json!({"model": opts.model, "prompt_chars": prompt_chars}),
            );

            let start = std::time::Instant::now();
            let delta_tx = spawn_progress_forwarder(&bridge, call_id.clone());
            let llm_result = vm_call_llm_full_streaming_offthread(&opts, delta_tx).await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let result = match llm_result {
                Ok(r) => r,
                Err(e) => {
                    bridge.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        "error",
                        serde_json::json!({"error": e.to_string()}),
                    );
                    return Err(e);
                }
            };

            trace_llm_call(LlmTraceEntry {
                model: result.model.clone(),
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                duration_ms,
            });

            bridge.send_call_end(
                &call_id,
                "llm",
                "llm_call",
                duration_ms,
                "ok",
                serde_json::json!({
                    "model": result.model,
                    "input_tokens": result.input_tokens,
                    "output_tokens": result.output_tokens,
                }),
            );

            let mut transcript_messages = opts.messages.clone();
            transcript_messages.push(build_assistant_response_message(
                &result.text,
                &result.blocks,
                &result.tool_calls,
                &opts.provider,
            ));
            let mut extra_events = vec![transcript_event(
                "provider_payload",
                "assistant",
                "internal",
                "",
                Some(serde_json::json!({
                    "model": result.model.clone(),
                    "input_tokens": result.input_tokens,
                    "output_tokens": result.output_tokens,
                    "tool_calls": result.tool_calls.clone(),
                })),
            )];
            if let Some(thinking) = result.thinking.clone() {
                if !thinking.is_empty() {
                    extra_events.push(transcript_event(
                        "private_reasoning",
                        "assistant",
                        "private",
                        &thinking,
                        None,
                    ));
                }
            }
            let transcript = transcript_to_vm_with_events(
                opts.transcript_id.clone(),
                opts.transcript_summary.clone(),
                opts.transcript_metadata.clone(),
                &transcript_messages,
                extra_events,
                Vec::new(),
                Some("active"),
            );

            // Always return dict (breaking change: no more plain string)
            if opts.response_format.as_deref() == Some("json") {
                let json_str = extract_json(&result.text);
                let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                    .ok()
                    .map(|jv| json_to_vm_value(&jv));
                return Ok(vm_build_llm_result(&result, parsed, Some(transcript)));
            }

            Ok(vm_build_llm_result(&result, None, Some(transcript)))
        }
    });
}
