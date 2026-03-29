use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;
use crate::vm::Vm;

use super::api::vm_call_llm_full;
use super::helpers::{
    opt_bool, opt_int, opt_str, vm_resolve_api_key, vm_resolve_model, vm_resolve_provider,
};
use super::tools::{
    build_assistant_tool_message, build_text_tool_prompt, build_tool_result_message,
    handle_tool_locally, normalize_tool_args, parse_text_tool_calls, resolve_tools_for_agent,
};
use super::trace::{trace_llm_call, LlmTraceEntry};

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
            let prompt = args.first().map(|a| a.display()).unwrap_or_default();
            let system = args.get(1).map(|a| a.display());
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();

            let provider = vm_resolve_provider(&options);
            let model = vm_resolve_model(&options, &provider);
            let api_key = vm_resolve_api_key(&provider)?;
            let max_iterations = opt_int(&options, "max_iterations").unwrap_or(25) as usize;
            let persistent = opt_bool(&options, "persistent");
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(5) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let max_tokens = opt_int(&options, "max_tokens").unwrap_or(8192);
            let tool_format = opt_str(&options, "tool_format")
                .unwrap_or_else(|| "text".to_string());

            // Resolve tool definitions from options.tools.
            // Accepts: string name list, tool_registry, or list of tool dicts.
            let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
            let tool_schemas = match &tools_val {
                Some(val) => resolve_tools_for_agent(val, &provider)?,
                None => None,
            };
            let has_tools = tool_schemas.as_ref().is_some_and(|t| !t.is_empty());

            eprintln!(
                "[agent_loop] model={}/{} prompt_chars={} sys_chars={} has_tools={} format={}",
                provider, model, prompt.len(),
                system.as_ref().map_or(0, |s| s.len()),
                has_tools, tool_format,
            );

            // Native format: pass tools via API. Text format: inject into prompt.
            let native_tools = if has_tools && tool_format == "native" {
                tool_schemas
            } else {
                None
            };

            let mut system_prompt = system.unwrap_or_default();

            // Always inject tool schema for text-based tool calling.
            // The schema lists tool names and parameters explicitly.
            // If the system prompt already has ```call examples, skip the format
            // instructions (the few-shot examples teach the format).
            if has_tools && tool_format == "text" {
                let has_examples = system_prompt.contains("```call");
                system_prompt.push_str(&build_text_tool_prompt(
                    tools_val.as_ref(),
                    !has_examples, // include format instructions only if no examples present
                ));
            }

            if persistent {
                system_prompt.push_str(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tools. \
                     When you are done, output ##DONE## on its own line.",
                );
            }

            let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
                "role": "user",
                "content": prompt,
            })];

            let mut total_text = String::new();
            let mut consecutive_text_only = 0usize;
            let mut all_tools_used: Vec<String> = Vec::new();
            let mut total_iterations = 0usize;
            let mut final_status = "done";
            let loop_start = std::time::Instant::now();

            for iteration in 0..max_iterations {
                total_iterations = iteration + 1;
                let start = std::time::Instant::now();
                let result = vm_call_llm_full(
                    &provider,
                    &model,
                    &api_key,
                    &messages,
                    if system_prompt.is_empty() {
                        None
                    } else {
                        Some(&system_prompt)
                    },
                    max_tokens,
                    None,
                    None,
                    None,
                    native_tools.as_deref(),
                )
                .await?;
                trace_llm_call(LlmTraceEntry {
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms: start.elapsed().as_millis() as u64,
                });

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
                        // Native: add assistant message with tool_calls metadata
                        messages.push(build_assistant_tool_message(
                            &text, &tool_calls, &provider,
                        ));
                    } else {
                        // Text: add the raw assistant text (contains <tool_call> tags)
                        messages.push(serde_json::json!({
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

                        // Try local handling first (read_file, list_directory),
                        // fall back to bridge for mutations and complex tools
                        let call_result = if let Some(local_result) =
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

                        let result_text = match call_result {
                            Ok(val) => {
                                if let Some(s) = val.as_str() {
                                    s.to_string()
                                } else if val.is_null() {
                                    "(no output)".to_string()
                                } else {
                                    serde_json::to_string_pretty(&val).unwrap_or_default()
                                }
                            }
                            Err(e) => format!("Error: {e}"),
                        };

                        if tool_format == "native" {
                            messages.push(build_tool_result_message(
                                tool_id, &result_text, &provider,
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
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": observations.trim_end(),
                        }));
                    }

                    continue; // Next iteration -- LLM sees tool results
                }

                // Text-only response (no tool calls found)
                messages.push(serde_json::json!({
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

                messages.push(serde_json::json!({
                    "role": "user",
                    "content": nudge,
                }));
            }

            // Return structured result for pipeline introspection.
            // Pipelines can access result.text, result.status, etc.
            let mut result_dict = BTreeMap::new();
            result_dict.insert("status".to_string(), VmValue::String(Rc::from(final_status)));
            result_dict.insert("text".to_string(), VmValue::String(Rc::from(total_text.as_str())));
            result_dict.insert("iterations".to_string(), VmValue::Int(total_iterations as i64));
            result_dict.insert("duration_ms".to_string(), VmValue::Int(loop_start.elapsed().as_millis() as i64));
            result_dict.insert(
                "tools_used".to_string(),
                VmValue::List(Rc::from(
                    all_tools_used.iter().map(|s| VmValue::String(Rc::from(s.as_str()))).collect::<Vec<_>>(),
                )),
            );
            Ok(VmValue::Dict(Rc::from(result_dict)))
        }
    });
}
