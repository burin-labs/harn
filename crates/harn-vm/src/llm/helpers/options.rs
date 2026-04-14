//! LLM call option extraction — parses the `(prompt, system, options)`
//! argument shape every high-level builtin accepts into the canonical
//! `LlmCallOptions` struct, including provider-specific warnings.

use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::{
    opt_bool, opt_float, opt_int, opt_str, resolve_api_key, transcript_id, transcript_message_list,
    transcript_metadata, transcript_summary_text, vm_message_list_to_json, vm_messages_to_json,
    vm_resolve_model, vm_resolve_provider, vm_value_dict_to_json, vm_value_to_json,
    TRANSCRIPT_TYPE,
};

pub(crate) fn extract_json(text: &str) -> String {
    crate::stdlib::json::extract_json_from_text(text)
}

pub(crate) fn expects_structured_output(opts: &crate::llm::api::LlmCallOptions) -> bool {
    opts.response_format.as_deref() == Some("json")
        || opts.json_schema.is_some()
        || opts.output_schema.is_some()
}

/// Extract all LLM call options from the standard (prompt, system, options) args.
pub(crate) fn extract_llm_options(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    use crate::llm::api::{LlmCallOptions, ThinkingConfig};
    use crate::llm::tools::vm_tools_to_native;

    let prompt = args.first().map(|a| a.display()).unwrap_or_default();
    let system = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let options = args.get(2).and_then(|a| a.as_dict()).cloned();

    let provider = vm_resolve_provider(&options);
    let model = vm_resolve_model(&options, &provider);
    let api_key = resolve_api_key(&provider)?;

    // Apply providers.toml model_defaults as fallbacks for unspecified params
    // (e.g. presence_penalty=1.5 for Qwen to avoid repetition loops).
    let model_defaults = crate::llm_config::model_params(&model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(|v| v.as_float()) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(|v| v.as_integer()) };

    let max_tokens = opt_int(&options, "max_tokens").unwrap_or(16384);
    let temperature = opt_float(&options, "temperature").or_else(|| default_float("temperature"));
    let top_p = opt_float(&options, "top_p").or_else(|| default_float("top_p"));
    let top_k = opt_int(&options, "top_k").or_else(|| default_int("top_k"));
    let stop = opt_str_list(&options, "stop");
    let seed = opt_int(&options, "seed");
    let frequency_penalty =
        opt_float(&options, "frequency_penalty").or_else(|| default_float("frequency_penalty"));
    let presence_penalty =
        opt_float(&options, "presence_penalty").or_else(|| default_float("presence_penalty"));
    let response_format = opt_str(&options, "response_format");
    let timeout = opt_int(&options, "timeout").map(|t| t as u64);
    let idle_timeout = opt_int(&options, "idle_timeout").map(|t| t as u64);
    let cache = opt_bool(&options, "cache");
    let stream = options
        .as_ref()
        .and_then(|o| o.get("stream"))
        .map(|v| v.is_truthy())
        .unwrap_or_else(|| {
            std::env::var("HARN_LLM_STREAM")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true)
        });
    let output_validation = opt_str(&options, "output_validation");

    let thinking = options
        .as_ref()
        .and_then(|o| o.get("thinking"))
        .and_then(|v| match v {
            VmValue::Bool(true) => Some(ThinkingConfig::Enabled),
            VmValue::Dict(d) => {
                let budget = d
                    .get("budget_tokens")
                    .and_then(|b| b.as_int())
                    .unwrap_or(10000);
                Some(ThinkingConfig::WithBudget(budget))
            }
            _ if v.is_truthy() => Some(ThinkingConfig::Enabled),
            _ => None,
        });

    let json_schema = options
        .as_ref()
        .and_then(|o| o.get("schema"))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);
    let output_schema = options
        .as_ref()
        .and_then(|o| o.get("output_schema").or_else(|| o.get("schema")))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);

    let transcript_val = options.as_ref().and_then(|o| o.get("transcript")).cloned();
    let transcript_dict = transcript_val
        .as_ref()
        .and_then(|v| v.as_dict())
        .filter(|d| d.get("_type").map(|v| v.display()).as_deref() == Some(TRANSCRIPT_TYPE));
    let transcript_id = transcript_dict.and_then(transcript_id);
    let transcript_summary = transcript_dict.and_then(transcript_summary_text);
    let transcript_metadata = transcript_dict.and_then(transcript_metadata);

    // Message source precedence: options.messages > transcript > prompt.
    let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();
    let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
        vm_messages_to_json(msg_list)?
    } else if let Some(transcript) = transcript_dict {
        let mut messages = vm_message_list_to_json(&transcript_message_list(transcript)?)?;
        if !prompt.is_empty() {
            messages.push(serde_json::json!({
                "role": "user",
                "content": prompt,
            }));
        }
        messages
    } else {
        vec![serde_json::json!({"role": "user", "content": prompt})]
    };

    let system = match (system, transcript_summary.clone()) {
        (Some(system), Some(summary)) => {
            Some(format!("{system}\n\nConversation summary:\n{summary}"))
        }
        (Some(system), None) => Some(system),
        (None, Some(summary)) => Some(format!("Conversation summary:\n{summary}")),
        (None, None) => None,
    };

    let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
    let native_tools = if let Some(tools) = &tools_val {
        Some(vm_tools_to_native(tools, &provider)?)
    } else {
        None
    };

    let tool_choice = options
        .as_ref()
        .and_then(|o| o.get("tool_choice"))
        .map(vm_value_to_json);

    let provider_overrides = options
        .as_ref()
        .and_then(|o| o.get(&provider))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);

    let opts = LlmCallOptions {
        provider,
        model,
        api_key,
        messages,
        system,
        transcript_id,
        transcript_summary,
        transcript_metadata,
        max_tokens,
        temperature,
        top_p,
        top_k,
        stop,
        seed,
        frequency_penalty,
        presence_penalty,
        response_format,
        json_schema,
        output_schema,
        output_validation,
        thinking,
        tools: tools_val,
        native_tools,
        tool_choice,
        cache,
        timeout,
        idle_timeout,
        stream,
        provider_overrides,
    };

    validate_options(&opts);
    Ok(opts)
}

pub(crate) fn opt_str_list(
    options: &Option<BTreeMap<String, VmValue>>,
    key: &str,
) -> Option<Vec<String>> {
    let val = options.as_ref()?.get(key)?;
    match val {
        VmValue::List(list) => {
            let strs: Vec<String> = list.iter().map(|v| v.display()).collect();
            if strs.is_empty() {
                None
            } else {
                Some(strs)
            }
        }
        _ => None,
    }
}

/// Emit warnings for options not supported by the target provider.
fn validate_options(opts: &crate::llm::api::LlmCallOptions) {
    let p = opts.provider.as_str();
    let warn = |param: &str| {
        crate::events::log_warn(
            "llm",
            &format!("\"{param}\" is not supported by provider \"{p}\", ignoring"),
        );
    };

    match p {
        "anthropic" => {
            if opts.seed.is_some() {
                warn("seed");
            }
            if opts.frequency_penalty.is_some() {
                warn("frequency_penalty");
            }
            if opts.presence_penalty.is_some() {
                warn("presence_penalty");
            }
        }
        "openai" | "openrouter" | "huggingface" | "local" => {
            if opts.top_k.is_some() {
                warn("top_k");
            }
            if opts.thinking.is_some() {
                warn("thinking");
            }
            if opts.cache {
                warn("cache");
            }
        }
        "ollama" => {
            if opts.frequency_penalty.is_some() {
                warn("frequency_penalty");
            }
            if opts.presence_penalty.is_some() {
                warn("presence_penalty");
            }
            if opts.cache {
                warn("cache");
            }
        }
        _ => {}
    }
}
