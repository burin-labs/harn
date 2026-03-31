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
pub(crate) mod api;
mod config_builtins;
mod conversation;
pub(crate) mod cost;
pub(crate) mod helpers;
mod mock;
mod stream;
mod tools;
mod trace;

use std::rc::Rc;
use std::sync::Arc;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmChannelHandle, VmValue};
use crate::vm::Vm;

use self::api::{vm_build_llm_result, vm_call_completion_full};
use self::helpers::{extract_json, opt_bool, opt_int, opt_str, transcript_to_vm};
use self::stream::vm_stream_llm;
use self::trace::trace_llm_call;

// =============================================================================
// Public re-exports (used by other crates/modules)
// =============================================================================

pub(crate) use self::agent::{
    agent_loop_result_from_llm, run_agent_loop_internal, AgentLoopConfig,
};
pub use self::agent::{register_agent_loop_with_bridge, register_llm_call_with_bridge};
pub(crate) use self::api::vm_call_llm_full;
pub(crate) use self::helpers::extract_llm_options;
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
        let opts = extract_llm_options(&args)?;

        let start = std::time::Instant::now();
        let result = vm_call_llm_full(&opts).await?;
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
        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });

        // If response_format is "json", parse the response
        if opts.response_format.as_deref() == Some("json") {
            let json_str = extract_json(&result.text);
            let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                .ok()
                .map(|jv| json_to_vm_value(&jv));
            return Ok(vm_build_llm_result(&result, parsed, Some(transcript)));
        }

        Ok(vm_build_llm_result(&result, None, Some(transcript)))
    });

    vm.register_async_builtin("llm_completion", |args| async move {
        let prefix = args.first().map(|a| a.display()).unwrap_or_default();
        let suffix = args.get(1).and_then(|a| {
            if matches!(a, VmValue::Nil) {
                None
            } else {
                Some(a.display())
            }
        });
        let opts = extract_llm_options(&[
            VmValue::String(Rc::from(prefix.clone())),
            args.get(2).cloned().unwrap_or(VmValue::Nil),
            args.get(3).cloned().unwrap_or(VmValue::Nil),
        ])?;

        let start = std::time::Instant::now();
        let result = vm_call_completion_full(&opts, &prefix, suffix.as_deref()).await?;
        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });
        Ok(vm_build_llm_result(&result, None, None))
    });

    // =========================================================================
    // agent_loop -- multi-turn persistent agent loop
    // =========================================================================
    vm.register_async_builtin("agent_loop", |args| async move {
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();
        let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
        let persistent = opt_bool(&options, "persistent");
        let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
        let custom_nudge = opt_str(&options, "nudge");
        let mut opts = extract_llm_options(&args)?;
        let result = run_agent_loop_internal(
            &mut opts,
            AgentLoopConfig {
                persistent,
                max_iterations,
                max_nudges,
                nudge: custom_nudge,
                tool_retries: 0,
                tool_backoff_ms: 1000,
                tool_format: "text".to_string(),
            },
        )
        .await?;
        Ok(json_to_vm_value(&result))
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
        let opts = extract_llm_options(&args)?;
        let provider = opts.provider.clone();
        let prompt_text = opts
            .messages
            .last()
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();

        let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(64);
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closed_clone = closed.clone();
        #[allow(clippy::arc_with_non_send_sync)]
        let tx_arc = Arc::new(tx);
        let tx_for_task = tx_arc.clone();

        tokio::task::spawn_local(async move {
            // Mock provider: send deterministic chunks without API call
            if provider == "mock" {
                let words: Vec<&str> = prompt_text.split_whitespace().collect();
                for word in &words {
                    let _ = tx_for_task.send(VmValue::String(Rc::from(*word))).await;
                }
                closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }

            let result = vm_stream_llm(&opts, &tx_for_task).await;
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
