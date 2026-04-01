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

use crate::stdlib::{json_to_vm_value, schema_result_value};
use crate::value::{VmChannelHandle, VmValue};
use crate::vm::Vm;

use self::api::{vm_build_llm_result, vm_call_completion_full};
use self::helpers::{
    extract_json, opt_bool, opt_int, opt_str, transcript_event, transcript_to_vm_with_events,
};
use self::stream::vm_stream_llm;
use self::tools::build_assistant_response_message;
use self::trace::trace_llm_call;

fn output_validation_mode(opts: &api::LlmCallOptions) -> &str {
    opts.output_validation.as_deref().unwrap_or("off")
}

fn schema_validation_errors(result: &VmValue) -> Vec<String> {
    match result {
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name == "Result" && variant == "Err" => fields
            .first()
            .and_then(|payload| payload.as_dict())
            .and_then(|payload| payload.get("errors"))
            .and_then(|errors| match errors {
                VmValue::List(items) => Some(items.iter().map(|err| err.display()).collect()),
                _ => None,
            })
            .unwrap_or_else(|| vec!["schema validation failed".to_string()]),
        _ => Vec::new(),
    }
}

fn validated_output_data(
    data: &VmValue,
    opts: &api::LlmCallOptions,
) -> Result<Option<VmValue>, VmValue> {
    let Some(schema_json) = &opts.output_schema else {
        return Ok(Some(data.clone()));
    };
    let schema_vm = json_to_vm_value(&normalize_validation_schema(schema_json.clone()));
    let validation = schema_result_value(data, &schema_vm, false);
    let errors = schema_validation_errors(&validation);
    if errors.is_empty() {
        return Ok(Some(data.clone()));
    }
    let message = format!("LLM output failed schema validation: {}", errors.join("; "));
    match output_validation_mode(opts) {
        "warn" => {
            eprintln!("[harn] warning: {message}");
            Ok(Some(data.clone()))
        }
        "error" => Err(VmValue::String(Rc::from(message))),
        _ => Ok(Some(data.clone())),
    }
}

fn normalize_validation_schema(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut normalized = serde_json::Map::new();
            for (key, child) in map {
                if key == "type" {
                    let normalized_type = match child.as_str() {
                        Some("object") => serde_json::Value::String("dict".to_string()),
                        Some("array") => serde_json::Value::String("list".to_string()),
                        Some("integer") => serde_json::Value::String("int".to_string()),
                        Some("number") => serde_json::Value::String("float".to_string()),
                        Some("boolean") => serde_json::Value::String("bool".to_string()),
                        _ => normalize_validation_schema(child),
                    };
                    normalized.insert(key, normalized_type);
                } else {
                    normalized.insert(key, normalize_validation_schema(child));
                }
            }
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(normalize_validation_schema).collect())
        }
        other => other,
    }
}

// =============================================================================
// Public re-exports (used by other crates/modules)
// =============================================================================

pub(crate) use self::agent::{
    agent_loop_result_from_llm, current_host_bridge, run_agent_loop_internal, AgentLoopConfig,
};
pub use self::agent::{register_agent_loop_with_bridge, register_llm_call_with_bridge};
pub(crate) use self::api::vm_call_llm_full;
pub(crate) use self::helpers::extract_llm_options;
pub use self::helpers::vm_value_to_json;
pub use self::mock::{set_replay_mode, LlmReplayMode};
pub use self::trace::{enable_tracing, peek_trace, peek_trace_summary, take_trace, LlmTraceEntry};

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
        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });

        // If response_format is "json", parse the response and optionally
        // validate it against a configured output contract.
        if opts.response_format.as_deref() == Some("json") {
            let json_str = extract_json(&result.text);
            let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                .ok()
                .map(|jv| json_to_vm_value(&jv));
            let validated = match parsed.as_ref() {
                Some(data) => match validated_output_data(data, &opts) {
                    Ok(value) => value,
                    Err(error) => return Err(crate::value::VmError::Thrown(error)),
                },
                None => parsed,
            };
            return Ok(vm_build_llm_result(&result, validated, Some(transcript)));
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
        let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
        let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
        let tool_format = opt_str(&options, "tool_format").unwrap_or_else(|| "text".to_string());
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
                tool_retries,
                tool_backoff_ms,
                tool_format,
                auto_compact,
                policy,
                daemon,
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

#[cfg(test)]
mod tests {
    use super::api::LlmCallOptions;
    use super::validated_output_data;
    use crate::value::VmValue;
    use std::rc::Rc;

    fn base_opts() -> LlmCallOptions {
        LlmCallOptions {
            provider: "mock".to_string(),
            model: "mock".to_string(),
            api_key: String::new(),
            messages: Vec::new(),
            system: None,
            transcript_id: None,
            transcript_summary: None,
            transcript_metadata: None,
            max_tokens: 128,
            temperature: None,
            top_p: None,
            top_k: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            response_format: Some("json".to_string()),
            json_schema: None,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            })),
            output_validation: Some("error".to_string()),
            thinking: None,
            tools: None,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            provider_overrides: None,
        }
    }

    #[test]
    fn output_validation_accepts_matching_schema() {
        let opts = base_opts();
        let mut map = std::collections::BTreeMap::new();
        map.insert("name".to_string(), VmValue::String(Rc::from("Ada")));
        let data = VmValue::Dict(Rc::new(map));
        let validated = validated_output_data(&data, &opts).expect("schema should pass");
        assert!(validated.is_some());
    }

    #[test]
    fn output_validation_rejects_mismatched_schema_in_error_mode() {
        let opts = base_opts();
        let mut map = std::collections::BTreeMap::new();
        map.insert("name".to_string(), VmValue::Int(42));
        let data = VmValue::Dict(Rc::new(map));
        let error = validated_output_data(&data, &opts).expect_err("schema should fail");
        assert!(error.display().contains("schema validation"));
    }
}
