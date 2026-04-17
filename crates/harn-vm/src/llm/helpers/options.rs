//! LLM call option extraction — parses the `(prompt, system, options)`
//! argument shape every high-level builtin accepts into the canonical
//! `LlmCallOptions` struct, including provider-specific warnings.

use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::{
    opt_bool, opt_float, opt_int, opt_str, resolve_api_key, vm_messages_to_json, vm_resolve_model,
    vm_resolve_provider, vm_value_dict_to_json, vm_value_to_json,
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
    use crate::llm::api::{LlmCallOptions, ThinkingConfig, ToolSearchMode, ToolSearchVariant};
    use crate::llm::provider::{provider_supports_defer_loading, provider_tool_search_variants};
    use crate::llm::tools::{
        apply_tool_search_native_injection, extract_deferred_tool_names, vm_tools_to_native,
    };

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

    // Reject the deprecated `transcript` option key. Conversation
    // lifecycle is expressed through `session_id` + the explicit
    // `agent_session_*` builtins; there is no opaque transcript dict to
    // pass around anymore.
    if options.as_ref().and_then(|o| o.get("transcript")).is_some() {
        return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "llm_call / agent_loop: the `transcript` option was removed. \
                 Open or open-and-resume a session with agent_session_open(id) \
                 and pass `session_id: id` instead.",
        ))));
    }

    // Message source precedence: options.messages > prompt.
    let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();
    let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
        vm_messages_to_json(msg_list)?
    } else {
        vec![serde_json::json!({"role": "user", "content": prompt})]
    };

    let tools_val = options.as_ref().and_then(|o| o.get("tools")).cloned();
    let mut native_tools = if let Some(tools) = &tools_val {
        Some(vm_tools_to_native(tools, &provider)?)
    } else {
        None
    };

    // tool_search option parsing: three shapes accepted.
    //   - shorthand string: "bm25" | "regex" (mode: auto)
    //   - bool: true (defaults to bm25/auto), false (no tool_search)
    //   - dict: { variant, mode, always_loaded }
    // Unset / false / nil all leave tool_search absent — tools ship eagerly.
    let tool_search = parse_tool_search_option(options.as_ref())?;

    if let Some(cfg) = &tool_search {
        // Resolve tool_search against the active provider now — in auto
        // mode, either promote to native (by injecting the meta-tool) or
        // error pointing at the client-executed fallback issue (harn#70).
        let native_variants = provider_tool_search_variants(&provider, &model);
        let provider_has_native =
            provider_supports_defer_loading(&provider, &model) && !native_variants.is_empty();
        let use_native = match cfg.mode {
            ToolSearchMode::Native => {
                if !provider_has_native {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        format!(
                            "tool_search: provider \"{provider}\" does not expose native \
                         tool-search for model \"{model}\". Set \
                         `tool_search: {{ mode: \"client\" }}` (see harn#70, pending) \
                         or omit tool_search to ship tools eagerly."
                        ),
                    ))));
                }
                true
            }
            ToolSearchMode::Client => {
                return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                    "tool_search: mode: \"client\" is not implemented yet (tracked in \
                     harn#70). Use mode: \"auto\" with a native-capable provider \
                     (Anthropic Opus/Sonnet 4.0+, Haiku 4.5+) or omit tool_search.",
                ))));
            }
            ToolSearchMode::Auto => {
                if !provider_has_native {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        format!(
                            "tool_search: provider \"{provider}\" / model \"{model}\" \
                         has no native tool-search and the client-executed fallback \
                         is not implemented yet (tracked in harn#70). Either switch \
                         to an Anthropic Claude 4.0+ Opus/Sonnet or Haiku 4.5+ model, \
                         or omit tool_search."
                        ),
                    ))));
                }
                true
            }
        };

        if use_native {
            // Confirm the requested variant is actually supported; downgrade
            // with a warning if not (e.g. user asked for "regex" but provider
            // only exposes "bm25" — unlikely today but future-proofs).
            if !native_variants.contains(&cfg.variant.as_short()) {
                crate::events::log_warn(
                    "llm.tool_search",
                    &format!(
                        "provider \"{provider}\" model \"{model}\" does not support \
                         tool_search variant \"{}\"; falling back to \"{}\"",
                        cfg.variant.as_short(),
                        native_variants[0],
                    ),
                );
            }

            // Pre-flight: Anthropic rejects all-deferred tool lists.
            if let Some(tools) = native_tools.as_ref() {
                let deferred = extract_deferred_tool_names(tools);
                let total_user_tools = tools.len();
                if total_user_tools > 0 && deferred.len() == total_user_tools {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search: all tools have defer_loading set. At least \
                         one tool must be non-deferred so the model has somewhere \
                         to start. (Matches Anthropic's 400 on the same condition.)",
                    ))));
                }
            }

            // Inject the native meta-tool at index 0 of native_tools.
            let effective_variant = if native_variants.contains(&cfg.variant.as_short()) {
                cfg.variant
            } else {
                match native_variants[0] {
                    "regex" => ToolSearchVariant::Regex,
                    _ => ToolSearchVariant::Bm25,
                }
            };
            apply_tool_search_native_injection(
                &mut native_tools,
                &provider,
                effective_variant.as_short(),
            );
        }
    }

    let tool_choice = options
        .as_ref()
        .and_then(|o| o.get("tool_choice"))
        .map(vm_value_to_json);

    let provider_overrides = options
        .as_ref()
        .and_then(|o| o.get(&provider))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);

    let prefill = options
        .as_ref()
        .and_then(|o| o.get("prefill"))
        .and_then(|v| {
            if matches!(v, VmValue::Nil) {
                None
            } else {
                let s = v.display();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            }
        });

    let opts = LlmCallOptions {
        provider,
        model,
        api_key,
        messages,
        system,
        transcript_summary: None,
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
        tool_search,
        cache,
        timeout,
        idle_timeout,
        stream,
        provider_overrides,
        prefill,
    };

    validate_options(&opts);
    Ok(opts)
}

/// Parse the `tool_search` option into a ToolSearchConfig.
///
/// Accepts:
///   - `nil` / absent / `false` → None (no tool_search engaged)
///   - `true` → default (bm25 + auto)
///   - `"bm25"` | `"regex"` → that variant + auto
///   - `{ variant?, mode?, always_loaded? }` → explicit
fn parse_tool_search_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Option<crate::llm::api::ToolSearchConfig>, VmError> {
    use crate::llm::api::{ToolSearchConfig, ToolSearchMode, ToolSearchVariant};

    let raw = match options.and_then(|o| o.get("tool_search")) {
        Some(v) => v,
        None => return Ok(None),
    };

    let variant_from_short = |s: &str| -> Result<ToolSearchVariant, VmError> {
        match s {
            "bm25" => Ok(ToolSearchVariant::Bm25),
            "regex" => Ok(ToolSearchVariant::Regex),
            other => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                format!("tool_search.variant: expected \"bm25\" or \"regex\", got \"{other}\""),
            )))),
        }
    };
    let mode_from_short = |s: &str| -> Result<ToolSearchMode, VmError> {
        match s {
            "auto" => Ok(ToolSearchMode::Auto),
            "native" => Ok(ToolSearchMode::Native),
            "client" => Ok(ToolSearchMode::Client),
            other => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                format!(
                "tool_search.mode: expected \"auto\" | \"native\" | \"client\", got \"{other}\""
            ),
            )))),
        }
    };

    match raw {
        VmValue::Nil => Ok(None),
        VmValue::Bool(false) => Ok(None),
        VmValue::Bool(true) => Ok(Some(ToolSearchConfig::default_bm25_auto())),
        VmValue::String(s) => Ok(Some(ToolSearchConfig {
            variant: variant_from_short(s.as_ref())?,
            mode: ToolSearchMode::Auto,
            always_loaded: Vec::new(),
        })),
        VmValue::Dict(d) => {
            let variant = match d.get("variant") {
                Some(VmValue::String(s)) => variant_from_short(s.as_ref())?,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.variant: expected a string",
                    ))));
                }
                None => ToolSearchVariant::Bm25,
            };
            let mode = match d.get("mode") {
                Some(VmValue::String(s)) => mode_from_short(s.as_ref())?,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.mode: expected a string",
                    ))));
                }
                None => ToolSearchMode::Auto,
            };
            let always_loaded = match d.get("always_loaded") {
                Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.always_loaded: expected a list of tool names",
                    ))));
                }
                None => Vec::new(),
            };
            Ok(Some(ToolSearchConfig {
                variant,
                mode,
                always_loaded,
            }))
        }
        _ => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "tool_search: expected bool, string (\"bm25\"/\"regex\"), or dict \
             ({variant, mode, always_loaded})",
        )))),
    }
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
