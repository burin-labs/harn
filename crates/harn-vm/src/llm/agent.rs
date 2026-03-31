use std::rc::Rc;

use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use super::api::{vm_call_llm_full_streaming, DeltaSender};
use super::helpers::{extract_llm_options, opt_bool, opt_int, opt_str, transcript_to_vm};
use super::tools::{
    build_assistant_tool_message, build_text_tool_prompt, build_tool_result_message,
    handle_tool_locally, normalize_tool_args, parse_text_tool_calls, resolve_tools_for_agent,
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
    pub tool_retries: usize,
    pub tool_backoff_ms: u64,
    pub tool_format: String,
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

async fn inject_queued_user_messages(
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    opts: &mut super::api::LlmCallOptions,
    include_interrupt_immediate: bool,
    include_finish_step: bool,
    include_wait_for_completion: bool,
) -> Result<Vec<String>, VmError> {
    let Some(bridge) = bridge else {
        return Ok(Vec::new());
    };
    let queued = bridge
        .take_queued_user_messages(
            include_interrupt_immediate,
            include_finish_step,
            include_wait_for_completion,
        )
        .await;
    let mut delivered = Vec::new();
    for message in queued {
        delivered.push(message.content.clone());
        opts.messages.push(serde_json::json!({
            "role": "user",
            "content": message.content,
        }));
    }
    Ok(delivered)
}

pub(crate) fn agent_loop_result_from_llm(
    result: &super::api::LlmResult,
    opts: super::api::LlmCallOptions,
) -> serde_json::Value {
    serde_json::json!({
        "status": "done",
        "text": result.text,
        "visible_text": result.text,
        "private_reasoning": result.thinking,
        "iterations": 1,
        "duration_ms": 0,
        "tools_used": [],
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm(
            opts.transcript_id,
            opts.transcript_summary,
            opts.transcript_metadata,
            &opts.messages,
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
    let bridge = current_host_bridge();
    let max_iterations = config.max_iterations;
    let persistent = config.persistent;
    let max_nudges = config.max_nudges;
    let custom_nudge = config.nudge;
    let tool_retries = config.tool_retries;
    let tool_backoff_ms = config.tool_backoff_ms;
    let tool_format = config.tool_format;

    let tools_val = None::<VmValue>;
    let tool_schemas = match &tools_val {
        Some(val) => resolve_tools_for_agent(val, &opts.provider)?,
        None => None,
    };
    let has_tools = !opts
        .native_tools
        .as_ref()
        .map(|v| v.is_empty())
        .unwrap_or(true)
        || tool_schemas.as_ref().is_some_and(|tools| !tools.is_empty());

    if has_tools && tool_format != "native" {
        opts.native_tools = None;
    }
    if has_tools && tool_format == "text" {
        let system_prompt = opts.system.get_or_insert_with(String::new);
        let has_examples = system_prompt.contains("```call");
        if !has_examples {
            system_prompt.push_str(&build_text_tool_prompt(None, true));
        }
    }

    if persistent {
        let system_prompt = opts.system.get_or_insert_with(String::new);
        system_prompt.push_str(
            "\n\nIMPORTANT: You MUST keep working until the task is complete. \
             Do NOT stop to explain or summarize — take action with tools. \
             When you are done, output ##DONE## on its own line.",
        );
    }

    let mut total_text = String::new();
    let mut consecutive_text_only = 0usize;
    let mut all_tools_used: Vec<String> = Vec::new();
    let mut rejected_tools: Vec<String> = Vec::new();
    let mut deferred_user_messages: Vec<String> = Vec::new();
    let mut total_iterations = 0usize;
    let mut final_status = "done";
    let loop_start = std::time::Instant::now();

    for iteration in 0..max_iterations {
        total_iterations = iteration + 1;
        let immediate_messages =
            inject_queued_user_messages(bridge.as_ref(), opts, true, false, false).await?;
        if !immediate_messages.is_empty() {
            consecutive_text_only = 0;
        }
        let start = std::time::Instant::now();
        let result = if let Some(bridge) = bridge.as_ref() {
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
                    result
                }
                Err(error) => {
                    bridge.send_call_end(
                        &llm_call_id,
                        "llm",
                        "llm_call",
                        llm_duration,
                        "error",
                        serde_json::json!({"error": error.to_string()}),
                    );
                    return Err(error);
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

        let tool_calls = if !result.tool_calls.is_empty() {
            result.tool_calls.clone()
        } else if has_tools && tool_format == "text" {
            parse_text_tool_calls(&text)
        } else {
            Vec::new()
        };

        if !tool_calls.is_empty() {
            consecutive_text_only = 0;
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
            for tc in &tool_calls {
                let tool_id = tc["id"].as_str().unwrap_or("");
                let tool_name = tc["name"].as_str().unwrap_or("");
                let tool_args = normalize_tool_args(tool_name, &tc["arguments"]);
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
            let finish_step_messages =
                inject_queued_user_messages(bridge.as_ref(), opts, true, true, false).await?;
            if !finish_step_messages.is_empty() {
                consecutive_text_only = 0;
            }
            continue;
        }

        opts.messages.push(serde_json::json!({
            "role": "assistant",
            "content": text,
        }));

        if persistent && text.contains("##DONE##") {
            break;
        }
        if !persistent {
            break;
        }

        let finish_step_messages =
            inject_queued_user_messages(bridge.as_ref(), opts, true, true, false).await?;
        if !finish_step_messages.is_empty() {
            consecutive_text_only = 0;
            continue;
        }

        consecutive_text_only += 1;
        if consecutive_text_only > max_nudges {
            final_status = "stuck";
            break;
        }

        let nudge = custom_nudge.clone().unwrap_or_else(|| {
            if consecutive_text_only == 1 {
                "You must use tools to complete this task. Start with the best available tool."
                    .to_string()
            } else if consecutive_text_only <= 3 {
                "STOP explaining and USE TOOLS NOW. Include a concrete tool call.".to_string()
            } else {
                "FINAL WARNING: call a tool now or the task will fail.".to_string()
            }
        });
        opts.messages.push(serde_json::json!({
            "role": "user",
            "content": nudge,
        }));
    }

    deferred_user_messages
        .extend(inject_queued_user_messages(bridge.as_ref(), opts, false, false, true).await?);

    Ok(serde_json::json!({
        "status": final_status,
        "text": total_text,
        "visible_text": total_text,
        "iterations": total_iterations,
        "duration_ms": loop_start.elapsed().as_millis() as i64,
        "tools_used": all_tools_used,
        "rejected_tools": rejected_tools,
        "deferred_user_messages": deferred_user_messages,
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm(
            opts.transcript_id.clone(),
            opts.transcript_summary.clone(),
            opts.transcript_metadata.clone(),
            &opts.messages,
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
            let mut opts = extract_llm_options(&args)?;
            let result = run_agent_loop_internal(
                &mut opts,
                AgentLoopConfig {
                    persistent,
                    max_iterations,
                    max_nudges,
                    nudge: custom_nudge,
                    tool_retries,
                    tool_backoff_ms,
                    tool_format,
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
    use super::helpers::{extract_json, transcript_to_vm};
    use crate::stdlib::json_to_vm_value;

    let b = bridge;
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            let opts = extract_llm_options(&args)?;

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
            let llm_result = vm_call_llm_full_streaming(&opts, delta_tx).await;
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
            transcript_messages.push(serde_json::json!({
                "role": "assistant",
                "content": result.text.clone(),
            }));
            let transcript = transcript_to_vm(
                opts.transcript_id.clone(),
                opts.transcript_summary.clone(),
                opts.transcript_metadata.clone(),
                &transcript_messages,
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
