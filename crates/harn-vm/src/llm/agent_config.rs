//! Agent loop configuration, builtin registration, and result building
//! extracted from `agent.rs` for maintainability.

use std::rc::Rc;

use crate::stdlib::harn_entry::register_harn_entrypoint_category;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

use super::agent_observe::{
    observed_llm_call, LlmRetryConfig, DEFAULT_LLM_CALL_BACKOFF_MS, DEFAULT_LLM_CALL_RETRIES,
};
use super::helpers::{
    extract_llm_options, opt_bool, opt_int, opt_str, system_prompt_event_metadata,
    system_prompt_metadata, transcript_event, transcript_to_vm_with_event_prefix,
};
use super::tools::build_assistant_response_message;

const AGENT_STDLIB_ENTRYPOINT_CATEGORY: &str = "agent.stdlib";
const PREFILL_ASSISTANT_FEEDBACK_KIND: &str = "prefill_assistant";

const AGENT_CONTROL_BUILTINS: &[&VmBuiltinDef] = &[
    &AGENT_SUBSCRIBE_BUILTIN_DEF,
    &AGENT_INJECT_FEEDBACK_BUILTIN_DEF,
];

pub(crate) fn agent_feedback_message(kind: &str, content: &str) -> VmValue {
    let mut msg = std::collections::BTreeMap::new();
    if kind == PREFILL_ASSISTANT_FEEDBACK_KIND {
        msg.insert("role".to_string(), VmValue::String(Rc::from("assistant")));
        msg.insert(
            "content".to_string(),
            VmValue::String(Rc::from(content.to_string())),
        );
        return VmValue::Dict(Rc::new(msg));
    }
    let body = format!("<runtime_feedback kind=\"{kind}\">\n{content}\n</runtime_feedback>");
    msg.insert("role".to_string(), VmValue::String(Rc::from("user")));
    msg.insert("content".to_string(), VmValue::String(Rc::from(body)));
    VmValue::Dict(Rc::new(msg))
}

fn system_prompt_transcript_metadata(system: Option<&String>) -> Option<serde_json::Value> {
    system
        .filter(|system| !system.trim().is_empty())
        .map(|system| serde_json::json!({ "system_prompt": system_prompt_metadata(system) }))
}

fn system_prompt_transcript_events(system: Option<&String>) -> Vec<VmValue> {
    system
        .filter(|system| !system.trim().is_empty())
        .map(|system| {
            vec![transcript_event(
                "system_prompt",
                "system",
                "internal",
                "",
                Some(system_prompt_event_metadata(system)),
            )]
        })
        .unwrap_or_default()
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
        result.thinking.as_deref(),
        &opts.provider,
        &opts.model,
    ));
    let transcript_metadata = system_prompt_transcript_metadata(opts.system.as_ref());
    let prefix_events = system_prompt_transcript_events(opts.system.as_ref());
    let mut events = Vec::new();
    events.push(transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
            "blocks": result.blocks.clone(),
            "provider_response_id": result.telemetry.request_id.clone(),
            "thinking_summary": result.thinking_summary,
            "cost_usd": crate::llm::cost::calculate_cost_for_provider(
                &result.provider,
                &result.model,
                result.input_tokens,
                result.output_tokens,
            ),
            "route_policy": opts.route_policy.as_label(),
            "routing_decision": opts.routing_decision.as_ref(),
            "structural_experiment": opts.applied_structural_experiment.as_ref(),
        })),
    ));
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
    if let Some(summary) = result.thinking_summary.clone() {
        if !summary.is_empty() {
            events.push(transcript_event(
                "thinking_summary",
                "assistant",
                "private",
                &summary,
                None,
            ));
        }
    }
    serde_json::json!({
        "status": "done",
        "text": result.text,
        "visible_text": result.text,
        "private_reasoning": result.thinking,
        "thinking_summary": result.thinking_summary,
        "llm": {
            "iterations": 1,
            "duration_ms": 0,
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
        },
        "tools": {
            "calls": [],
            "successful": [],
            "rejected": [],
            "mode": "",
        },
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_event_prefix(
            None,
            opts.transcript_summary,
            transcript_metadata,
            &transcript_messages,
            prefix_events,
            events,
            Vec::new(),
            Some("active"),
        )),
    })
}

/// Assemble the user-facing result dict for `llm_call` from a raw `LlmResult`.
pub(crate) fn build_llm_call_result(
    result: &super::api::LlmResult,
    opts: &super::api::LlmCallOptions,
) -> VmValue {
    use super::api::vm_build_llm_result;
    use super::helpers::{expects_structured_output, extract_json};
    use crate::stdlib::json_to_vm_value;

    let mut transcript_messages = opts.messages.clone();
    transcript_messages.push(build_assistant_response_message(
        &result.text,
        &result.blocks,
        &result.tool_calls,
        result.thinking.as_deref(),
        &opts.provider,
        &opts.model,
    ));
    let transcript_metadata = system_prompt_transcript_metadata(opts.system.as_ref());
    let prefix_events = system_prompt_transcript_events(opts.system.as_ref());
    let mut extra_events = Vec::new();
    extra_events.push(transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
            "blocks": result.blocks.clone(),
            "provider_response_id": result.telemetry.request_id.clone(),
            "thinking_summary": result.thinking_summary,
            "structural_experiment": opts.applied_structural_experiment.as_ref(),
        })),
    ));
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
    if let Some(summary) = result.thinking_summary.clone() {
        if !summary.is_empty() {
            extra_events.push(transcript_event(
                "thinking_summary",
                "assistant",
                "private",
                &summary,
                None,
            ));
        }
    }
    let transcript = transcript_to_vm_with_event_prefix(
        None,
        opts.transcript_summary.clone(),
        transcript_metadata,
        &transcript_messages,
        prefix_events,
        extra_events,
        Vec::new(),
        Some("active"),
    );

    if expects_structured_output(opts) {
        let parsed = structured_output_candidates(result, opts.tools.as_ref())
            .into_iter()
            .find_map(|candidate| {
                let json_str = extract_json(&candidate);
                serde_json::from_str::<serde_json::Value>(&json_str)
                    .ok()
                    .map(|jv| json_to_vm_value(&jv))
            });
        return vm_build_llm_result(result, parsed, Some(transcript), opts.tools.as_ref());
    }

    vm_build_llm_result(result, None, Some(transcript), opts.tools.as_ref())
}

fn structured_output_candidates(
    result: &super::api::LlmResult,
    tools: Option<&crate::value::VmValue>,
) -> Vec<String> {
    let mut candidates = Vec::new();
    push_structured_output_candidate(&mut candidates, result.text.trim().to_string());

    let public_blocks = result
        .blocks
        .iter()
        .filter(|block| {
            block.get("type").and_then(|value| value.as_str()) == Some("output_text")
                && block.get("visibility").and_then(|value| value.as_str()) != Some("private")
        })
        .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
        .collect::<String>();
    push_structured_output_candidate(&mut candidates, public_blocks.trim().to_string());

    for call in &result.tool_calls {
        if let Some(arguments) = call.get("arguments") {
            if let Ok(serialized) = serde_json::to_string(arguments) {
                push_structured_output_candidate(&mut candidates, serialized);
            }
        }
    }

    let derived = candidates.clone();
    for candidate in derived {
        let parsed = crate::llm::tools::parse_text_tool_calls_with_tools(&candidate, tools);
        if !parsed.prose.is_empty() {
            push_structured_output_candidate(&mut candidates, parsed.prose.trim().to_string());
        }
    }

    candidates
}

fn push_structured_output_candidate(candidates: &mut Vec<String>, candidate: String) {
    if candidate.is_empty() || candidates.iter().any(|existing| existing == &candidate) {
        return;
    }
    candidates.push(candidate);
}

pub(crate) fn register_agent_loop(vm: &mut Vm) {
    register_harn_entrypoint_category(vm, AGENT_STDLIB_ENTRYPOINT_CATEGORY);
}

pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    super::agent_runtime::install_current_host_bridge(bridge);
    register_harn_entrypoint_category(vm, AGENT_STDLIB_ENTRYPOINT_CATEGORY);
}

pub(crate) fn register_agent_control_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, AGENT_CONTROL_BUILTINS);
}

/// Subscribe a Harn callback to events for an agent session.
#[harn_builtin(
    sig = "agent_subscribe(session_id: string, callback: closure) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn agent_subscribe_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "agent_subscribe(session_id, callback): session_id must be a string".into(),
            ))
        }
    };
    let callback = args.get(1).cloned().ok_or_else(|| {
        VmError::Runtime("agent_subscribe(session_id, callback): callback closure required".into())
    })?;
    if !matches!(callback, VmValue::Closure(_)) {
        return Err(VmError::Runtime(
            "agent_subscribe(session_id, callback): callback must be a closure".into(),
        ));
    }
    crate::agent_sessions::append_subscriber(&session_id, callback);
    Ok(VmValue::Nil)
}

/// Inject pending feedback into an agent session.
#[harn_builtin(
    sig = "agent_inject_feedback(session_id: string, kind: string, content: string) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn agent_inject_feedback_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "agent_inject_feedback(session_id, kind, content): session_id must be a string"
                    .into(),
            ))
        }
    };
    let kind = match args.get(1) {
        Some(VmValue::String(s)) => s.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "agent_inject_feedback(session_id, kind, content): kind must be a string".into(),
            ))
        }
    };
    let content = match args.get(2) {
        Some(VmValue::String(s)) => s.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "agent_inject_feedback(session_id, kind, content): content must be a string".into(),
            ))
        }
    };
    crate::agent_sessions::inject_message(&session_id, agent_feedback_message(&kind, &content))
        .map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Register a bridge-aware `llm_call` that emits call_start/call_end notifications.
pub fn register_llm_call_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    let metadata = VmBuiltinMetadata::async_static("llm_call")
        .signature_static("llm_call(prompt, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .category_static("llm.host")
        .doc_static("Execute one bridge-observed LLM call and return the normalized result dict.");
    vm.register_async_builtin_with_metadata(metadata, move |args| {
        let bridge = b.clone();
        async move {
            let mut opts = extract_llm_options(&args)?;
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let user_visible = opt_bool(&options, "user_visible");
            // Match the non-bridge `llm_call` default (see
            // `crate::llm::execute_llm_call`): fail fast unless the caller
            // opts into transient HTTP/provider retries.
            let retry_config = LlmRetryConfig {
                retries: opt_int(&options, "llm_retries")
                    .unwrap_or(DEFAULT_LLM_CALL_RETRIES as i64)
                    .max(0) as usize,
                backoff_ms: opt_int(&options, "llm_backoff_ms")
                    .unwrap_or(DEFAULT_LLM_CALL_BACKOFF_MS as i64)
                    .max(0) as u64,
            };
            let _ =
                crate::llm::structural_experiments::apply_structural_experiment(&mut opts, None)
                    .await?;

            let result = observed_llm_call(
                &opts,
                opt_str(&options, "tool_format").as_deref(),
                Some(&bridge),
                &retry_config,
                None,
                user_visible,
                true,
                // Direct `llm_call` host invocations are not part of an
                // agent loop, so the streaming candidate detector
                // (harn#692) doesn't fire here.
                None,
            )
            .await?;

            Ok(build_llm_call_result(&result, &opts))
        }
    });
}

/// Register bridge-aware `llm_call_structured` / `llm_call_structured_safe`.
/// The bridge path still runs the schema-retry loop locally so the
/// throws-on-exhausted-retries contract matches the non-bridge path;
/// the bridge receives per-attempt call_start/call_end notifications
/// identically to the plain `llm_call` bridge variant. Paired with
/// `register_llm_call_with_bridge` in the ACP setup.
pub fn register_llm_call_structured_with_bridge(
    vm: &mut Vm,
    bridge: Rc<crate::bridge::HostBridge>,
) {
    let b1 = bridge.clone();
    let structured = VmBuiltinMetadata::async_static("llm_call_structured")
        .signature_static("llm_call_structured(prompt, schema, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .category_static("llm.structured")
        .doc_static("Call an LLM through the bridge for schema-valid JSON data.");
    vm.register_async_builtin_with_metadata(structured, move |args| {
        let bridge = b1.clone();
        async move {
            let rewritten = crate::llm::rewrite_structured_args(args)?;
            let opts = extract_llm_options(&rewritten)?;
            let options = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
            let response = crate::llm::execute_llm_call(opts, options, Some(&bridge)).await?;
            Ok(crate::llm::extract_structured_data(response))
        }
    });
    let b2 = bridge.clone();
    let structured_safe = VmBuiltinMetadata::async_static("llm_call_structured_safe")
        .signature_static("llm_call_structured_safe(prompt, schema, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .category_static("llm.structured")
        .doc_static("Call an LLM through the bridge and return a non-throwing schema envelope.");
    vm.register_async_builtin_with_metadata(structured_safe, move |args| {
        let bridge = b2.clone();
        async move {
            let rewritten = match crate::llm::rewrite_structured_args(args) {
                Ok(v) => v,
                Err(err) => return Ok(crate::llm::structured_safe_envelope_err(&err)),
            };
            let opts = match extract_llm_options(&rewritten) {
                Ok(opts) => opts,
                Err(err) => return Ok(crate::llm::structured_safe_envelope_err(&err)),
            };
            let options = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
            match crate::llm::execute_llm_call(opts, options, Some(&bridge)).await {
                Ok(response) => Ok(crate::llm::structured_safe_envelope_ok(
                    crate::llm::extract_structured_data(response),
                )),
                Err(err) => Ok(crate::llm::structured_safe_envelope_err(&err)),
            }
        }
    });
    let b3 = bridge;
    let structured_result = VmBuiltinMetadata::async_static("llm_call_structured_result")
        .signature_static("llm_call_structured_result(prompt, schema, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .category_static("llm.structured")
        .doc_static(
            "Call an LLM through the bridge and return a diagnostic structured-output envelope.",
        );
    vm.register_async_builtin_with_metadata(structured_result, move |args| {
        let bridge = b3.clone();
        async move {
            crate::llm::structured_envelope::llm_call_structured_result_impl(args, Some(&bridge))
                .await
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_output_candidates_include_tool_call_arguments() {
        let result = crate::llm::api::LlmResult {
            text: String::new(),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "type": "tool_call",
                "name": "json_response",
                "arguments": {"answer": "ok"},
            })],
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            model: "claude-sonnet-4-6".to_string(),
            provider: "anthropic".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            logprobs: Vec::new(),
            blocks: Vec::new(),
            telemetry: crate::llm::api::ProviderTelemetry::default(),
        };

        let candidates = structured_output_candidates(&result, None);

        assert!(candidates
            .iter()
            .any(|candidate| candidate == r#"{"answer":"ok"}"#));
    }
}
