//! LLM integration: API calls, streaming, agent loops, tool handling, and tracing.
//!
//! This module is split into sub-modules for maintainability:
//! - `api`: Core LLM API call logic (request building, response parsing)
//! - `agent`: Agent loop implementations (basic and bridge-backed)
//! - `stream`: SSE streaming support
//! - `tools`: Tool schema resolution, text-based tool calling, argument normalization
//! - `mock`: Mock provider and fixture record/replay
//! - `trace`: LLM call tracing (thread-local trace log)
//! - `helpers`: Option extraction, provider/model/key resolution, JSON conversion
//! - `conversation`: Conversation management builtins
//! - `config_builtins`: Provider configuration query builtins

mod agent;
mod api;
mod config_builtins;
mod conversation;
pub(crate) mod cost;
mod helpers;
mod mock;
mod stream;
mod tools;
mod trace;

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmChannelHandle, VmValue};
use crate::vm::Vm;

use self::api::{vm_build_llm_result, vm_call_llm_full};
use self::helpers::{
    extract_json, opt_bool, opt_float, opt_int, opt_str, vm_messages_to_json, vm_resolve_api_key,
    vm_resolve_model, vm_resolve_provider,
};
use self::stream::vm_stream_llm;
use self::tools::vm_tools_to_native;
use self::trace::trace_llm_call;

// =============================================================================
// Public re-exports (used by other crates/modules)
// =============================================================================

pub use self::agent::{register_agent_loop_with_bridge, register_llm_call_with_bridge};
pub use self::helpers::vm_value_to_json;
pub use self::mock::{set_replay_mode, LlmReplayMode};
pub use self::trace::{enable_tracing, peek_trace_summary, take_trace, LlmTraceEntry};

/// Reset all thread-local LLM state (cost, trace, mock). Call between test runs.
pub fn reset_llm_state() {
    cost::reset_cost_state();
    trace::reset_trace_state();
}

/// Register LLM builtins on a VM.
pub fn register_llm_builtins(vm: &mut Vm) {
    // =========================================================================
    // llm_call -- core LLM request with structured output + tool use
    // =========================================================================
    vm.register_async_builtin("llm_call", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);
        let response_format = opt_str(&options, "response_format");
        let json_schema = options
            .as_ref()
            .and_then(|o| o.get("schema"))
            .and_then(|v| v.as_dict())
            .cloned();
        let temperature = opt_float(&options, "temperature");
        let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
        let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();

        // Build messages -- either from messages option or from prompt
        let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
            vm_messages_to_json(msg_list)?
        } else {
            vec![serde_json::json!({"role": "user", "content": prompt})]
        };

        // Build native tool definitions from tool_registry or tool list
        let native_tools = if let Some(tools) = &tools_val {
            Some(vm_tools_to_native(tools, &provider)?)
        } else {
            None
        };

        let start = std::time::Instant::now();
        let result = vm_call_llm_full(
            &provider,
            &model,
            &api_key,
            &messages,
            system.as_deref(),
            max_tokens,
            response_format.as_deref(),
            json_schema.as_ref(),
            temperature,
            native_tools.as_deref(),
        )
        .await?;
        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });

        // If response_format is "json", parse the response
        if response_format.as_deref() == Some("json") {
            // Find JSON in the response (may have markdown code fences)
            let json_str = extract_json(&result.text);
            let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                .ok()
                .map(|jv| json_to_vm_value(&jv));
            return Ok(vm_build_llm_result(&result, parsed));
        }

        // If tool_use blocks are present, return structured result
        if !result.tool_calls.is_empty() {
            return Ok(vm_build_llm_result(&result, None));
        }

        // Backward compat: if no special options were used, return plain string
        // This matches the old llm_call(prompt, system) two-arg API
        let is_simple_call = tools_val.is_none() && messages_val.is_none();
        if is_simple_call {
            return Ok(VmValue::String(Rc::from(result.text)));
        }

        Ok(vm_build_llm_result(&result, None))
    });

    // =========================================================================
    // agent_loop -- multi-turn persistent agent loop
    // =========================================================================
    vm.register_async_builtin("agent_loop", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
        let persistent = opt_bool(&options, "persistent");
        let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
        let custom_nudge = opt_str(&options, "nudge");
        let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);

        let mut system_prompt = system.unwrap_or_default();
        if persistent {
            system_prompt.push_str(
                "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                 Do NOT stop to explain or summarize — take action. \
                 Output ##DONE## only when the task is fully complete and verified.",
            );
        }

        let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
            "role": "user",
            "content": prompt,
        })];

        let mut total_text = String::new();
        let mut consecutive_text_only = 0usize;
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
                None,
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
                final_status = "stuck";
                break;
            }

            let nudge = custom_nudge.clone().unwrap_or_else(|| {
                "You have not output ##DONE## yet — the task is not complete. \
                 Use your tools to continue working. \
                 Only output ##DONE## when the task is fully complete and verified."
                    .to_string()
            });

            messages.push(serde_json::json!({
                "role": "user",
                "content": nudge,
            }));
        }

        let mut result_dict = BTreeMap::new();
        result_dict.insert(
            "status".to_string(),
            VmValue::String(Rc::from(final_status)),
        );
        result_dict.insert("text".to_string(), VmValue::String(Rc::from(total_text)));
        result_dict.insert(
            "iterations".to_string(),
            VmValue::Int(total_iterations as i64),
        );
        result_dict.insert(
            "duration_ms".to_string(),
            VmValue::Int(loop_start.elapsed().as_millis() as i64),
        );
        result_dict.insert(
            "tools_used".to_string(),
            VmValue::List(Rc::from(Vec::<VmValue>::new())),
        );
        Ok(VmValue::Dict(Rc::from(result_dict)))
    });

    // Remaining builtins (llm_stream, conversation management, config, cost)
    register_llm_stream(vm);
    conversation::register_conversation_builtins(vm);
    config_builtins::register_config_builtins(vm);
    cost::register_cost_builtins(vm);
}

/// Register llm_stream builtin.
fn register_llm_stream(vm: &mut Vm) {
    vm.register_async_builtin("llm_stream", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_tokens = opt_int(&options, "max_tokens").unwrap_or(4096);

        let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(64);
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closed_clone = closed.clone();
        #[allow(clippy::arc_with_non_send_sync)]
        let tx_arc = Arc::new(tx);
        let tx_for_task = tx_arc.clone();

        tokio::task::spawn_local(async move {
            // Mock provider: send deterministic chunks without API call
            if provider == "mock" {
                let words: Vec<&str> = prompt.split_whitespace().collect();
                for word in &words {
                    let _ = tx_for_task.send(VmValue::String(Rc::from(*word))).await;
                }
                closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }

            let result = vm_stream_llm(
                &provider,
                &model,
                &api_key,
                &prompt,
                system.as_deref(),
                max_tokens,
                &tx_for_task,
            )
            .await;
            closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Err(e) = result {
                let _ = tx_for_task
                    .send(VmValue::String(Rc::from(format!("error: {e}"))))
                    .await;
            }
        });

        #[allow(clippy::arc_with_non_send_sync)]
        let handle = VmChannelHandle {
            name: "llm_stream".to_string(),
            sender: tx_arc,
            receiver: Arc::new(tokio::sync::Mutex::new(rx)),
            closed,
        };
        Ok(VmValue::Channel(handle))
    });
}
