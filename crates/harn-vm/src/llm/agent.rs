use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use super::api::{vm_call_llm_full_streaming, DeltaSender};
use super::helpers::{
    extract_llm_options, opt_bool, opt_int, opt_str,
};
use super::tools::{
    build_assistant_tool_message, build_text_tool_prompt, build_tool_result_message,
    handle_tool_locally, normalize_tool_args, parse_text_tool_calls, resolve_tools_for_agent,
};
use super::trace::{trace_llm_call, LlmTraceEntry};

fn next_call_id() -> String {
    uuid::Uuid::now_v7().to_string()
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

/// Register a tool-aware `agent_loop` that uses a bridge for tool execution.
/// This overrides the native text-only agent_loop with one that can:
/// 1. Pass tool definitions to the LLM
/// 2. Execute tool calls via the bridge (delegated to host)
/// 3. Feed tool results back into the conversation
pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    vm.register_async_builtin("agent_loop", move |args| {
        let bridge = b.clone();
        async move {
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
            let persistent = opt_bool(&options, "persistent");
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
            let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
            let tool_format = opt_str(&options, "tool_format")
                .unwrap_or_else(|| "text".to_string());

            // Extract base LLM options (provider, model, key, messages, etc.)
            let mut opts = extract_llm_options(&args)?;

            // Resolve tool definitions from options.tools for text-based injection.
            let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
            let tool_schemas = match &tools_val {
                Some(val) => resolve_tools_for_agent(val, &opts.provider)?,
                None => None,
            };
            let has_tools = tool_schemas.as_ref().is_some_and(|t| !t.is_empty());

            let prompt = args.first().map(|a| a.display()).unwrap_or_default();
            eprintln!(
                "[agent_loop] model={}/{} prompt_chars={} sys_chars={} has_tools={} format={}",
                opts.provider, opts.model, prompt.len(),
                opts.system.as_ref().map_or(0, |s| s.len()),
                has_tools, tool_format,
            );

            // Native format: pass tools via API. Text format: inject into prompt.
            if has_tools && tool_format != "native" {
                // Clear native_tools since we're using text format
                opts.native_tools = None;
            }

            // Always inject tool schema for text-based tool calling.
            if has_tools && tool_format == "text" {
                let system_prompt = opts.system.get_or_insert_with(String::new);
                let has_examples = system_prompt.contains("```call");
                system_prompt.push_str(&build_text_tool_prompt(
                    tools_val.as_ref(),
                    !has_examples,
                ));
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
            let mut total_iterations = 0usize;
            let mut final_status = "done";
            let loop_start = std::time::Instant::now();

            for iteration in 0..max_iterations {
                total_iterations = iteration + 1;
                bridge.send_progress(
                    "agent_loop",
                    &format!("Iteration {}", iteration + 1),
                    Some((iteration + 1) as i64),
                    Some(max_iterations as i64),
                    None,
                );
                let llm_call_id = next_call_id();
                let prompt_chars: usize = opts.messages.iter()
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
                let start = std::time::Instant::now();
                let delta_tx = spawn_progress_forwarder(&bridge, llm_call_id.clone());
                let llm_result = vm_call_llm_full_streaming(
                    &opts,
                    delta_tx,
                )
                .await;
                let llm_duration = start.elapsed().as_millis() as u64;
                let result = match llm_result {
                    Ok(r) => r,
                    Err(e) => {
                        bridge.send_call_end(
                            &llm_call_id, "llm", "llm_call", llm_duration, "error",
                            serde_json::json!({"error": e.to_string()}),
                        );
                        return Err(e);
                    }
                };
                trace_llm_call(LlmTraceEntry {
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms: llm_duration,
                });
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

                let text = result.text.clone();
                total_text.push_str(&text);

                // Collect tool calls from native API or text parsing
                let tool_calls = if !result.tool_calls.is_empty() {
                    result.tool_calls.clone()
                } else if has_tools && tool_format == "text" {
                    parse_text_tool_calls(&text)
                } else {
                    Vec::new()
                };

                // Handle tool calls
                if !tool_calls.is_empty() {
                    consecutive_text_only = 0;

                    if tool_format == "native" {
                        opts.messages.push(build_assistant_tool_message(
                            &text, &tool_calls, &opts.provider,
                        ));
                    } else {
                        opts.messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text,
                        }));
                    }

                    // Execute each tool call
                    let mut observations = String::new();
                    let mut tools_used_this_iter: Vec<String> = Vec::new();
                    for tc in &tool_calls {
                        let tool_id = tc["id"].as_str().unwrap_or("");
                        let tool_name = tc["name"].as_str().unwrap_or("");
                        let tool_args = normalize_tool_args(tool_name, &tc["arguments"]);

                        tools_used_this_iter.push(tool_name.to_string());
                        eprintln!(
                            "[agent_loop] iter={iteration} tool={tool_name} args={}",
                            serde_json::to_string(&tool_args).unwrap_or_default()
                        );

                        let tool_call_id = next_call_id();
                        bridge.send_call_start(
                            &tool_call_id,
                            "tool",
                            tool_name,
                            serde_json::json!({"iteration": iteration}),
                        );
                        let tool_start = std::time::Instant::now();

                        // Try local handling first (read_file, list_directory),
                        // fall back to bridge for mutations and complex tools.
                        // Retry on failure with exponential backoff if configured.
                        let call_result = {
                            let mut attempt = 0usize;
                            loop {
                                let result = if let Some(local_result) =
                                    handle_tool_locally(tool_name, &tool_args)
                                {
                                    Ok(serde_json::Value::String(local_result))
                                } else {
                                    bridge
                                        .call(
                                            "builtin_call",
                                            serde_json::json!({
                                                "name": tool_name,
                                                "args": [tool_args],
                                            }),
                                        )
                                        .await
                                };
                                match &result {
                                    Ok(_) => break result,
                                    // Never retry rejections — they are permanent
                                    Err(VmError::CategorizedError { category: ErrorCategory::ToolRejected, .. }) => {
                                        break result;
                                    }
                                    Err(_) if attempt < tool_retries => {
                                        attempt += 1;
                                        let delay = tool_backoff_ms * (1u64 << attempt.min(5));
                                        eprintln!(
                                            "[agent_loop] tool={tool_name} retry {attempt}/{tool_retries} backoff={delay}ms"
                                        );
                                        tokio::time::sleep(
                                            tokio::time::Duration::from_millis(delay),
                                        )
                                        .await;
                                    }
                                    Err(_) => break result,
                                }
                            }
                        };

                        let tool_ok = call_result.is_ok();
                        let is_rejected = matches!(
                            &call_result,
                            Err(VmError::CategorizedError { category: ErrorCategory::ToolRejected, .. })
                        );
                        let result_text = match &call_result {
                            Ok(val) => {
                                if let Some(s) = val.as_str() {
                                    s.to_string()
                                } else if val.is_null() {
                                    "(no output)".to_string()
                                } else {
                                    serde_json::to_string_pretty(val).unwrap_or_default()
                                }
                            }
                            Err(VmError::CategorizedError { message, category: ErrorCategory::ToolRejected }) => {
                                format!("REJECTED: {message} Do not retry this tool.")
                            }
                            Err(e) => format!("Error: {e}"),
                        };
                        if is_rejected && !rejected_tools.contains(&tool_name.to_string()) {
                            rejected_tools.push(tool_name.to_string());
                        }
                        let tool_status = if tool_ok { "ok" } else if is_rejected { "rejected" } else { "error" };
                        let tool_meta = if tool_ok {
                            serde_json::json!({})
                        } else {
                            serde_json::json!({"error": result_text})
                        };
                        bridge.send_call_end(
                            &tool_call_id,
                            "tool",
                            tool_name,
                            tool_start.elapsed().as_millis() as u64,
                            tool_status,
                            tool_meta,
                        );

                        if tool_format == "native" {
                            opts.messages.push(build_tool_result_message(
                                tool_id, &result_text, &opts.provider,
                            ));
                        } else {
                            observations.push_str(&format!(
                                "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
                            ));
                        }
                    }

                    all_tools_used.extend(tools_used_this_iter);

                    // Text format: send all observations as one user message
                    if tool_format != "native" && !observations.is_empty() {
                        opts.messages.push(serde_json::json!({
                            "role": "user",
                            "content": observations.trim_end(),
                        }));
                    }

                    continue; // Next iteration -- LLM sees tool results
                }

                // Text-only response (no tool calls found)
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

                consecutive_text_only += 1;
                if consecutive_text_only > max_nudges {
                    eprintln!(
                        "[agent_loop] max nudges ({max_nudges}) reached after {iteration} iterations"
                    );
                    final_status = "stuck";
                    break;
                }

                // Escalating nudge: more directive each time
                let nudge = custom_nudge.clone().unwrap_or_else(|| {
                    if consecutive_text_only == 1 {
                        "You must use tools to complete this task. \
                         Use ```call blocks to invoke tools. Example:\n\
                         ```call\n\
                         read_file(path=\"relevant_file.py\")\n\
                         ```\n\
                         Start by reading a file relevant to the task."
                            .to_string()
                    } else if consecutive_text_only <= 3 {
                        "STOP explaining and USE TOOLS NOW. \
                         You have tools: read_file, search, edit, run. \
                         Call them with ```call blocks. \
                         If the task requires creating a file, use:\n\
                         ```call\n\
                         edit(action=\"create\", path=\"file.py\", content=\"\"\"your code\"\"\")\n\
                         ```\n\
                         Do NOT respond with text only — include a tool call."
                            .to_string()
                    } else {
                        "FINAL WARNING: You MUST use a ```call block NOW or the task will fail. \
                         Pick the single most important action and do it."
                            .to_string()
                    }
                });

                opts.messages.push(serde_json::json!({
                    "role": "user",
                    "content": nudge,
                }));
            }

            // Return structured result for pipeline introspection.
            // Pipelines can access result.text, result.status, etc.
            let mut result_dict = BTreeMap::new();
            result_dict.insert("status".to_string(), VmValue::String(Rc::from(final_status)));
            result_dict.insert("text".to_string(), VmValue::String(Rc::from(total_text)));
            result_dict.insert("iterations".to_string(), VmValue::Int(total_iterations as i64));
            result_dict.insert("duration_ms".to_string(), VmValue::Int(loop_start.elapsed().as_millis() as i64));
            result_dict.insert(
                "tools_used".to_string(),
                VmValue::List(Rc::from(
                    all_tools_used.iter().map(|s| VmValue::String(Rc::from(s.as_str()))).collect::<Vec<_>>(),
                )),
            );
            result_dict.insert(
                "rejected_tools".to_string(),
                VmValue::List(Rc::from(
                    rejected_tools.iter().map(|s| VmValue::String(Rc::from(s.as_str()))).collect::<Vec<_>>(),
                )),
            );
            Ok(VmValue::Dict(Rc::from(result_dict)))
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
            let opts = extract_llm_options(&args)?;

            let call_id = next_call_id();
            let prompt_chars: usize = opts.messages
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

            // Always return dict (breaking change: no more plain string)
            if opts.response_format.as_deref() == Some("json") {
                let json_str = extract_json(&result.text);
                let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                    .ok()
                    .map(|jv| json_to_vm_value(&jv));
                return Ok(vm_build_llm_result(&result, parsed));
            }

            Ok(vm_build_llm_result(&result, None))
        }
    });
}
