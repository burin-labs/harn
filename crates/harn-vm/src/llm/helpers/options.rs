//! LLM call option extraction — parses the `(prompt, system, options)`
//! argument shape every high-level builtin accepts into the canonical
//! `LlmCallOptions` struct, including provider-specific warnings.

use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::{
    opt_bool, opt_float, opt_int, opt_str, provider_key_available, resolve_api_key,
    vm_messages_to_json, vm_resolve_model, vm_resolve_provider, vm_value_dict_to_json,
    vm_value_to_json,
};

pub(crate) fn extract_json(text: &str) -> String {
    crate::stdlib::json::extract_json_from_text(text)
}

pub(crate) fn expects_structured_output(opts: &crate::llm::api::LlmCallOptions) -> bool {
    opts.output_format.is_structured() || opts.output_schema.is_some()
}

fn quality_rank(tier: &str) -> i32 {
    match tier.to_ascii_lowercase().as_str() {
        "small" => 0,
        "mid" | "medium" => 1,
        "frontier" | "large" => 2,
        _ => 1,
    }
}

fn route_target_from_short(target: &str) -> Result<(String, String), crate::value::VmError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "route_policy: target must not be empty",
        ))));
    }
    if let Some((provider, model)) = target.split_once(':') {
        let provider_known = provider == "mock"
            || crate::llm_config::provider_config(provider).is_some()
            || crate::llm::provider::is_provider_registered(provider);
        if provider_known && !model.trim().is_empty() {
            let (resolved_model, _) = crate::llm_config::resolve_model(model.trim());
            return Ok((resolved_model, provider.trim().to_string()));
        }
    }
    let resolved = crate::llm_config::resolve_model_info(target);
    Ok((resolved.id, resolved.provider))
}

fn parse_route_policy_text(text: &str) -> Result<crate::llm::api::LlmRoutePolicy, VmError> {
    use crate::llm::api::LlmRoutePolicy;
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let arg = |name: &str| -> Option<String> {
        lower
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('('))
            .and_then(|rest| rest.strip_suffix(')'))
            .map(|_| text[name.len() + 1..text.len() - 1].trim().to_string())
    };
    if text.is_empty() || lower == "manual" {
        return Ok(LlmRoutePolicy::Manual);
    }
    if let Some(target) = arg("always") {
        return Ok(LlmRoutePolicy::Always(target));
    }
    if let Some(target) = arg("cheapest_over_quality") {
        return Ok(LlmRoutePolicy::CheapestOverQuality(target));
    }
    if let Some(target) = arg("fastest_over_quality") {
        return Ok(LlmRoutePolicy::FastestOverQuality(target));
    }
    Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(format!(
        "route_policy: expected manual, always(id), cheapest_over_quality(t), or fastest_over_quality(t), got {text:?}"
    )))))
}

fn vm_string_list(value: &VmValue) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |text: String| {
        let text = text.trim().to_string();
        if !text.is_empty() && !out.iter().any(|existing| existing == &text) {
            out.push(text);
        }
    };
    match value {
        VmValue::List(items) => {
            for item in items.iter() {
                push(item.display());
            }
        }
        VmValue::String(text) => {
            for item in text.split(',') {
                push(item.to_string());
            }
        }
        other => push(other.display()),
    }
    out
}

fn parse_route_policy_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<crate::llm::api::LlmRoutePolicy, VmError> {
    use crate::llm::api::LlmRoutePolicy;
    let Some(raw) = options.and_then(|o| o.get("route_policy")) else {
        if let Some(prefer) = options.and_then(|o| o.get("prefer")) {
            let targets = vm_string_list(prefer);
            if !targets.is_empty() {
                let strategy = options
                    .and_then(|o| o.get("fallback_strategy").or_else(|| o.get("strategy")))
                    .map(|value| value.display())
                    .unwrap_or_else(|| "prefer_order".to_string());
                return Ok(LlmRoutePolicy::PreferenceList { targets, strategy });
            }
        }
        return Ok(LlmRoutePolicy::Manual);
    };
    match raw {
        VmValue::Nil => Ok(LlmRoutePolicy::Manual),
        VmValue::Bool(false) => Ok(LlmRoutePolicy::Manual),
        VmValue::String(text) => parse_route_policy_text(text),
        VmValue::Dict(d) => {
            let mode = d
                .get("mode")
                .map(|value| value.display())
                .unwrap_or_else(|| "manual".to_string());
            let target = d
                .get("target")
                .or_else(|| d.get("quality"))
                .or_else(|| d.get("id"))
                .map(|value| value.display())
                .unwrap_or_default();
            match mode.as_str() {
                "manual" => Ok(LlmRoutePolicy::Manual),
                "always" => Ok(LlmRoutePolicy::Always(target)),
                "cheapest_over_quality" => Ok(LlmRoutePolicy::CheapestOverQuality(target)),
                "fastest_over_quality" => Ok(LlmRoutePolicy::FastestOverQuality(target)),
                "preference_list" | "prefer" => {
                    let targets = d
                        .get("targets")
                        .or_else(|| d.get("prefer"))
                        .map(vm_string_list)
                        .unwrap_or_default();
                    if targets.is_empty() {
                        return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                            "route_policy.prefer: expected at least one model/provider target",
                        ))));
                    }
                    let strategy = d
                        .get("strategy")
                        .or_else(|| d.get("fallback_strategy"))
                        .map(|value| value.display())
                        .unwrap_or_else(|| "prefer_order".to_string());
                    Ok(LlmRoutePolicy::PreferenceList { targets, strategy })
                }
                other => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                    format!("route_policy.mode: unsupported value {other:?}"),
                )))),
            }
        }
        _ => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "route_policy: expected string or dict",
        )))),
    }
}

fn parse_fallback_chain_option(options: Option<&BTreeMap<String, VmValue>>) -> Vec<String> {
    let Some(raw) = options.and_then(|o| o.get("fallback_chain")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |value: String| {
        let value = value.trim().to_string();
        if !value.is_empty() && !out.iter().any(|existing| existing == &value) {
            out.push(value);
        }
    };
    match raw {
        VmValue::List(list) => {
            for item in list.iter() {
                push(item.display());
            }
        }
        VmValue::String(text) => {
            for item in text.split(',') {
                push(item.to_string());
            }
        }
        _ => {}
    }
    out
}

fn route_alternative(
    provider: String,
    model: String,
    selected: bool,
    reason: String,
) -> crate::llm::api::LlmRouteAlternative {
    let quality_tier = crate::llm_config::model_tier(&model);
    let pricing = crate::llm::cost::pricing_per_1k_for(&provider, &model);
    crate::llm::api::LlmRouteAlternative {
        available: provider_key_available(&provider),
        cost_per_1k_in: pricing.map(|p| p.0),
        cost_per_1k_out: pricing.map(|p| p.1),
        latency_p50_ms: crate::llm::cost::latency_p50_ms_for(&provider),
        provider,
        model,
        quality_tier,
        selected,
        reason,
    }
}

fn resolve_route_policy(
    policy: &crate::llm::api::LlmRoutePolicy,
    current_provider: &str,
    current_model: &str,
) -> Result<Option<crate::llm::api::LlmRoutingDecision>, VmError> {
    use crate::llm::api::{LlmRoutePolicy, LlmRoutingDecision};

    match policy {
        LlmRoutePolicy::Manual => Ok(None),
        LlmRoutePolicy::Always(target) => {
            let (model, provider) = route_target_from_short(target)?;
            Ok(Some(LlmRoutingDecision {
                policy: policy.as_label(),
                requested_quality: None,
                selected_provider: provider.clone(),
                selected_model: model.clone(),
                alternatives: vec![route_alternative(
                    provider,
                    model,
                    true,
                    "pinned by always".to_string(),
                )],
            }))
        }
        LlmRoutePolicy::CheapestOverQuality(target)
        | LlmRoutePolicy::FastestOverQuality(target) => {
            let requested_rank = quality_rank(target);
            let mut alternatives = crate::llm_config::all_model_candidates()
                .into_iter()
                .filter(|(model, _)| {
                    quality_rank(&crate::llm_config::model_tier(model)) >= requested_rank
                })
                .map(|(model, provider)| {
                    route_alternative(provider, model, false, "candidate".to_string())
                })
                .collect::<Vec<_>>();

            if alternatives.is_empty() {
                alternatives.push(route_alternative(
                    current_provider.to_string(),
                    current_model.to_string(),
                    false,
                    "fallback_current_route".to_string(),
                ));
            }

            let score_cost = |alt: &crate::llm::api::LlmRouteAlternative| -> f64 {
                alt.cost_per_1k_in.unwrap_or(f64::INFINITY)
                    + alt.cost_per_1k_out.unwrap_or(f64::INFINITY)
            };
            let selected_idx = alternatives
                .iter()
                .enumerate()
                .filter(|(_, alt)| alt.available)
                .min_by(|(_, left), (_, right)| {
                    let left_score = match policy {
                        LlmRoutePolicy::CheapestOverQuality(_) => score_cost(left),
                        LlmRoutePolicy::FastestOverQuality(_) => {
                            left.latency_p50_ms.unwrap_or(u64::MAX) as f64
                        }
                        _ => unreachable!(),
                    };
                    let right_score = match policy {
                        LlmRoutePolicy::CheapestOverQuality(_) => score_cost(right),
                        LlmRoutePolicy::FastestOverQuality(_) => {
                            right.latency_p50_ms.unwrap_or(u64::MAX) as f64
                        }
                        _ => unreachable!(),
                    };
                    left_score
                        .partial_cmp(&right_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| left.provider.cmp(&right.provider))
                        .then_with(|| left.model.cmp(&right.model))
                })
                .map(|(idx, _)| idx)
                .unwrap_or(0);

            alternatives[selected_idx].selected = true;
            alternatives[selected_idx].reason = "selected".to_string();
            let selected = alternatives[selected_idx].clone();
            Ok(Some(LlmRoutingDecision {
                policy: policy.as_label(),
                requested_quality: Some(target.clone()),
                selected_provider: selected.provider,
                selected_model: selected.model,
                alternatives,
            }))
        }
        LlmRoutePolicy::PreferenceList { targets, strategy } => {
            let mut alternatives = Vec::new();
            for target in targets {
                let (model, provider) = route_target_from_short(target)?;
                if alternatives
                    .iter()
                    .any(|alt: &crate::llm::api::LlmRouteAlternative| {
                        alt.provider == provider && alt.model == model
                    })
                {
                    continue;
                }
                alternatives.push(route_alternative(
                    provider,
                    model,
                    false,
                    "candidate".to_string(),
                ));
            }
            if alternatives.is_empty() {
                alternatives.push(route_alternative(
                    current_provider.to_string(),
                    current_model.to_string(),
                    false,
                    "fallback_current_route".to_string(),
                ));
            }
            let normalized = strategy.trim().to_ascii_lowercase();
            let score_cost = |alt: &crate::llm::api::LlmRouteAlternative| -> f64 {
                alt.cost_per_1k_in.unwrap_or(f64::INFINITY)
                    + alt.cost_per_1k_out.unwrap_or(f64::INFINITY)
            };
            let selected_idx = alternatives
                .iter()
                .enumerate()
                .filter(|(_, alt)| alt.available)
                .min_by(
                    |(left_idx, left), (right_idx, right)| match normalized.as_str() {
                        "cheapest_first" | "cheapest" => score_cost(left)
                            .partial_cmp(&score_cost(right))
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| left_idx.cmp(right_idx)),
                        "fastest_first" | "fastest" => left
                            .latency_p50_ms
                            .unwrap_or(u64::MAX)
                            .cmp(&right.latency_p50_ms.unwrap_or(u64::MAX))
                            .then_with(|| left_idx.cmp(right_idx)),
                        _ => left_idx.cmp(right_idx),
                    },
                )
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            alternatives[selected_idx].selected = true;
            alternatives[selected_idx].reason = "selected".to_string();
            let selected = alternatives[selected_idx].clone();
            Ok(Some(LlmRoutingDecision {
                policy: policy.as_label(),
                requested_quality: None,
                selected_provider: selected.provider,
                selected_model: selected.model,
                alternatives,
            }))
        }
    }
}

/// Three-way resolution of `tool_search.mode` against the provider's
/// native capability. Kept as a private enum so the option-parse path
/// reads linearly; the `Client` variant feeds the harn#70 fallback
/// injection, the `Native` variant feeds the phase-1 Anthropic path
/// (and phase-2 OpenAI path via harn#71).
enum ToolSearchResolution {
    Native,
    Client,
}

/// Read the `provider_overrides.force_native_tool_search` escape hatch
/// (bool). Set to true when a user is pointed at a proxied OpenAI-compat
/// endpoint (self-hosted router, enterprise gateway) whose model ID
/// Harn cannot parse but that is known to forward `tool_search` +
/// `defer_loading` unchanged.
fn provider_overrides_force_native(
    options: Option<&BTreeMap<String, VmValue>>,
    provider: &str,
) -> bool {
    let Some(options) = options else { return false };
    let Some(VmValue::Dict(overrides)) = options.get(provider) else {
        return false;
    };
    matches!(
        overrides.get("force_native_tool_search"),
        Some(VmValue::Bool(true))
    )
}

/// Decide which wire shape this (provider, model) pair should emit for
/// the native tool-search meta-tool. Anthropic + Claude → Anthropic
/// shape; anything else → OpenAI shape. For `provider: "mock"` we
/// inspect the model string so conformance tests can spoof either
/// backend without HTTP.
fn classify_native_shape(
    provider: &str,
    model: &str,
) -> crate::llm::provider::NativeToolSearchShape {
    use crate::llm::provider::NativeToolSearchShape;
    if provider == "anthropic" {
        return NativeToolSearchShape::Anthropic;
    }
    if provider == "mock"
        && crate::llm::providers::anthropic::claude_model_supports_tool_search(model)
    {
        return NativeToolSearchShape::Anthropic;
    }
    NativeToolSearchShape::OpenAi
}

fn parse_schema_value(
    raw: Option<&VmValue>,
    field: &str,
) -> Result<Option<serde_json::Value>, VmError> {
    match raw {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => value
            .as_dict()
            .map(vm_value_dict_to_json)
            .map(Some)
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(std::rc::Rc::from(format!(
                    "{field}: expected a JSON Schema object"
                ))))
            }),
    }
}

fn output_format_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::rc::Rc::from(message.into())))
}

fn parse_output_format_kind(raw: &str) -> Result<&'static str, VmError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "text" | "none" | "off" => Ok("text"),
        "json" | "json_object" => Ok("json_object"),
        "json_schema" | "schema" => Ok("json_schema"),
        other => Err(output_format_error(format!(
            "output_format.kind: expected \"text\" | \"json_object\" | \"json_schema\", got \"{other}\""
        ))),
    }
}

fn parse_output_format_option(
    options: Option<&BTreeMap<String, VmValue>>,
    legacy_response_format: Option<&str>,
    legacy_json_schema: Option<&serde_json::Value>,
) -> Result<crate::llm::api::OutputFormat, VmError> {
    use crate::llm::api::OutputFormat;

    let Some(raw) = options.and_then(|o| o.get("output_format")) else {
        if let Some(schema) = legacy_json_schema {
            return Ok(OutputFormat::JsonSchema {
                schema: schema.clone(),
                strict: true,
            });
        }
        return match legacy_response_format {
            Some("json") | Some("json_object") => Ok(OutputFormat::JsonObject),
            Some("text") | None => Ok(OutputFormat::Text),
            Some(other) => Err(output_format_error(format!(
                "response_format: expected \"json\", \"json_object\", or \"text\", got \"{other}\""
            ))),
        };
    };

    match raw {
        VmValue::Nil => Ok(OutputFormat::Text),
        VmValue::String(kind) => match parse_output_format_kind(kind)? {
            "text" => Ok(OutputFormat::Text),
            "json_object" => Ok(OutputFormat::JsonObject),
            "json_schema" => {
                let Some(schema) = legacy_json_schema else {
                    return Err(output_format_error(
                        "output_format: kind \"json_schema\" requires a `schema` field",
                    ));
                };
                Ok(OutputFormat::JsonSchema {
                    schema: schema.clone(),
                    strict: true,
                })
            }
            _ => unreachable!(),
        },
        VmValue::Dict(d) => {
            let kind_raw = d
                .get("kind")
                .map(|value| value.display())
                .unwrap_or_else(|| "text".to_string());
            match parse_output_format_kind(&kind_raw)? {
                "text" => Ok(OutputFormat::Text),
                "json_object" => Ok(OutputFormat::JsonObject),
                "json_schema" => {
                    let schema = parse_schema_value(
                        d.get("schema").or_else(|| d.get("json_schema")),
                        "output_format.schema",
                    )?
                    .ok_or_else(|| {
                        output_format_error(
                            "output_format: kind \"json_schema\" requires a `schema` field",
                        )
                    })?;
                    let strict = d.get("strict").map(VmValue::is_truthy).unwrap_or(true);
                    Ok(OutputFormat::JsonSchema { schema, strict })
                }
                _ => unreachable!(),
            }
        }
        _ => Err(output_format_error(
            "output_format: expected string or dict",
        )),
    }
}

fn validate_output_format_supported(
    output_format: &crate::llm::api::OutputFormat,
    provider: &str,
    model: &str,
) -> Result<(), VmError> {
    use crate::llm::api::OutputFormat;

    match output_format {
        OutputFormat::Text => Ok(()),
        OutputFormat::JsonObject => Ok(()),
        OutputFormat::JsonSchema { strict, .. } => {
            if provider == "mock" {
                return Ok(());
            }
            let strategy = crate::llm::capabilities::lookup(provider, model).structured_output;
            match strategy.as_deref() {
                Some("native" | "tool_use" | "format_kw") => Ok(()),
                Some(other) => Err(output_format_error(format!(
                    "output_format: provider \"{provider}\" model \"{model}\" declares unsupported structured_output strategy \"{other}\""
                ))),
                None => {
                    let strict_msg = if *strict { " strict" } else { "" };
                    Err(output_format_error(format!(
                        "output_format: provider \"{provider}\" model \"{model}\" cannot enforce{strict_msg} json_schema output"
                    )))
                }
            }
        }
    }
}

/// Extract all LLM call options from the standard (prompt, system, options) args.
pub(crate) fn extract_llm_options(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    use crate::llm::api::{LlmCallOptions, ToolSearchMode, ToolSearchVariant};
    use crate::llm::provider::{
        provider_supports_defer_loading, provider_thinking_modes, provider_tool_search_variants,
    };
    use crate::llm::tools::{extract_deferred_tool_names, vm_tools_to_native};

    let prompt = args.first().map(|a| a.display()).unwrap_or_default();
    let system = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let explicit_options = args.get(2).and_then(|a| a.as_dict()).cloned();
    let options = crate::llm::cost_route::merge_context_options(explicit_options);

    let route_policy = parse_route_policy_option(options.as_ref())?;
    let mut provider = vm_resolve_provider(&options);
    let mut model = vm_resolve_model(&options, &provider);
    let routing_decision = resolve_route_policy(&route_policy, &provider, &model)?;
    if let Some(decision) = routing_decision.as_ref() {
        provider = decision.selected_provider.clone();
        model = decision.selected_model.clone();
    }
    let route_fallbacks = match &route_policy {
        crate::llm::api::LlmRoutePolicy::PreferenceList { .. } => routing_decision
            .as_ref()
            .map(|decision| {
                decision
                    .alternatives
                    .iter()
                    .filter(|alt| !alt.selected)
                    .map(|alt| crate::llm::api::LlmRouteFallback {
                        provider: alt.provider.clone(),
                        model: alt.model.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let fallback_chain = parse_fallback_chain_option(options.as_ref());
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

    let thinking = parse_thinking_option(options.as_ref())?;
    validate_thinking_supported(
        &thinking,
        &provider,
        &model,
        &provider_thinking_modes(&provider, &model),
    )?;
    let anthropic_beta_features =
        parse_anthropic_beta_features_option(options.as_ref(), &thinking, &provider, &model)?;

    let response_format = opt_str(&options, "response_format");
    let json_schema = parse_schema_value(
        options
            .as_ref()
            .and_then(|o| o.get("json_schema").or_else(|| o.get("schema"))),
        "json_schema",
    )?;
    let output_schema = parse_schema_value(
        options.as_ref().and_then(|o| {
            o.get("output_schema")
                .or_else(|| o.get("json_schema"))
                .or_else(|| o.get("schema"))
        }),
        "output_schema",
    )?;
    let output_format = parse_output_format_option(
        options.as_ref(),
        response_format.as_deref(),
        json_schema.as_ref(),
    )?;
    validate_output_format_supported(&output_format, &provider, &model)?;
    let output_schema = output_schema.or_else(|| output_format.schema().cloned());

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
    let vision =
        opt_bool(&options, "vision") || crate::llm::content::messages_contain_images(&messages)?;
    if vision && !crate::llm::capabilities::lookup(&provider, &model).vision_supported {
        return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(format!(
            "llm_call: provider \"{provider}\" model \"{model}\" does not declare vision_supported=true"
        )))));
    }
    if vision
        && provider == "ollama"
        && crate::llm::content::messages_contain_url_images(&messages)?
    {
        return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "llm_call: provider \"ollama\" requires image base64; url image content is not supported",
        ))));
    }

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
    let mut tool_search = parse_tool_search_option(options.as_ref())?;

    if let Some(cfg) = tool_search.as_mut() {
        // Resolve tool_search against the active provider now. Three
        // possible outcomes:
        //   - native: prepend the provider's meta-tool (Anthropic path
        //     for Claude 4.0+; OpenAI Responses-API path for GPT 5.4+).
        //   - client: keep native_tools as-is so the agent loop can
        //     strip deferred tools per-turn and inject the synthetic
        //     `__harn_tool_search` dispatchable.
        //   - error: explicit native mode on a provider that cannot
        //     satisfy it.
        let native_variants = provider_tool_search_variants(&provider, &model);
        let model_based_native =
            provider_supports_defer_loading(&provider, &model) && !native_variants.is_empty();
        // Escape hatch for proxied OpenAI-compat providers whose model
        // ID Harn cannot parse. The override forces the OpenAI
        // Responses-API shape; user asserts the endpoint forwards
        // `tool_search` + `defer_loading` unchanged.
        let forced = provider_overrides_force_native(options.as_ref(), &provider);
        let provider_has_native = model_based_native || forced;
        // If the forced path is active, use OpenAI's default variants
        // so the injection below picks the right shape.
        let effective_variants: Vec<String> = if forced && native_variants.is_empty() {
            vec!["hosted".to_string(), "client".to_string()]
        } else {
            native_variants
        };
        let variant_supported = |v: &str| effective_variants.iter().any(|x| x == v);
        let resolution = match cfg.mode {
            ToolSearchMode::Native => {
                if !provider_has_native {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        format!(
                            "tool_search: provider \"{provider}\" does not expose native \
                         tool-search for model \"{model}\". Set \
                         `tool_search: {{ mode: \"client\" }}` to use the client-executed \
                         fallback, or omit tool_search to ship tools eagerly."
                        ),
                    ))));
                }
                ToolSearchResolution::Native
            }
            ToolSearchMode::Client => ToolSearchResolution::Client,
            ToolSearchMode::Auto => {
                if provider_has_native {
                    ToolSearchResolution::Native
                } else {
                    ToolSearchResolution::Client
                }
            }
        };

        // Pre-flight (applies to both native and client): all-deferred
        // tool lists leave the model with no starting point. Anthropic
        // returns HTTP 400 on this and we match the diagnostic for
        // consistency across modes.
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

        match resolution {
            ToolSearchResolution::Native => {
                // Classify the native wire shape for this provider so
                // the injection and response parser agree on what to
                // emit / look for. Anthropic path emits the
                // `tool_search_tool_*_20251119` meta-tool; OpenAI path
                // emits `{"type": "tool_search"}`. For the "mock"
                // provider we infer from the model string so
                // conformance tests can exercise both paths without
                // HTTP. See `provider_native_tool_search_shape`.
                let shape = classify_native_shape(&provider, &model);
                match shape {
                    crate::llm::provider::NativeToolSearchShape::Anthropic => {
                        // Anthropic exposes {bm25, regex}. Variant
                        // names are documented in
                        // `effective_variants`; fall back to element 0
                        // with a warn if the user asked for something
                        // this model doesn't support.
                        if !variant_supported(cfg.variant.as_short()) {
                            crate::events::log_warn(
                                "llm.tool_search",
                                &format!(
                                    "provider \"{provider}\" model \"{model}\" does not support \
                                     tool_search variant \"{}\"; falling back to \"{}\"",
                                    cfg.variant.as_short(),
                                    effective_variants[0],
                                ),
                            );
                        }
                        let effective_variant = if variant_supported(cfg.variant.as_short()) {
                            cfg.variant
                        } else {
                            match effective_variants[0].as_str() {
                                "regex" => ToolSearchVariant::Regex,
                                _ => ToolSearchVariant::Bm25,
                            }
                        };
                        crate::llm::tools::apply_tool_search_native_injection_typed(
                            &mut native_tools,
                            shape,
                            effective_variant.as_short(),
                            "hosted",
                        );
                    }
                    crate::llm::provider::NativeToolSearchShape::OpenAi => {
                        // OpenAI Responses API exposes hosted + client
                        // modes. When the user picked `mode: "native"`
                        // they meant "let OpenAI handle the search on
                        // their side" — the hosted mode. Users who want
                        // Harn to execute the search locally should
                        // write `mode: "client"`, which flows through
                        // the harn#70 synthetic-tool path below (same
                        // ergonomics across every provider).
                        crate::llm::tools::apply_tool_search_native_injection_typed(
                            &mut native_tools,
                            shape,
                            cfg.variant.as_short(),
                            "hosted",
                        );
                    }
                }
            }
            ToolSearchResolution::Client => {
                // Client mode: capture the deferred tool bodies into
                // cfg.deferred_bodies (so the agent loop can re-surface
                // them), inject the synthetic search tool, and hide the
                // deferred tools from the initial payload. The agent
                // loop is responsible for promoting hits back onto
                // `opts.native_tools` across turns; for single-shot
                // `llm_call` the model still sees the synthetic tool
                // but without multi-turn continuity it degrades to one
                // query + one batch of suggestions.
                if let Some(list) = native_tools.as_ref() {
                    for tool in list {
                        let is_deferred = tool
                            .get("defer_loading")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if !is_deferred {
                            continue;
                        }
                        let name = tool
                            .get("name")
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                tool.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("")
                            .to_string();
                        if name.is_empty() {
                            continue;
                        }
                        // Strip `defer_loading` from the stored copy —
                        // providers that don't support the flag will
                        // reject it when the tool is later promoted.
                        let mut cloned = tool.clone();
                        if let Some(obj) = cloned.as_object_mut() {
                            obj.remove("defer_loading");
                        }
                        if let Some(function) =
                            cloned.get_mut("function").and_then(|v| v.as_object_mut())
                        {
                            function.remove("defer_loading");
                        }
                        cfg.deferred_bodies.insert(name, cloned);
                    }
                }
                crate::llm::tools::apply_tool_search_client_injection(
                    &mut native_tools,
                    &provider,
                    cfg,
                );
            }
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
    let structural_experiment =
        crate::llm::structural_experiments::parse_structural_experiment_option(options.as_ref())?;
    let budget = crate::llm::cost::parse_budget_envelope(options.as_ref())?;

    let opts = LlmCallOptions {
        provider,
        model,
        api_key,
        route_policy,
        fallback_chain,
        route_fallbacks,
        routing_decision,
        session_id: None,
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
        output_format,
        response_format,
        json_schema,
        output_schema,
        output_validation,
        thinking,
        anthropic_beta_features,
        vision,
        tools: tools_val,
        native_tools,
        tool_choice,
        tool_search,
        cache,
        timeout,
        idle_timeout,
        stream,
        provider_overrides,
        budget,
        prefill,
        structural_experiment,
        applied_structural_experiment: None,
    };

    validate_options(&opts);
    Ok(opts)
}

fn thinking_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::rc::Rc::from(message.into())))
}

fn parse_reasoning_effort(raw: &str) -> Result<crate::llm::api::ReasoningEffort, VmError> {
    match raw {
        "low" => Ok(crate::llm::api::ReasoningEffort::Low),
        "medium" => Ok(crate::llm::api::ReasoningEffort::Medium),
        "high" => Ok(crate::llm::api::ReasoningEffort::High),
        other => Err(thinking_error(format!(
            "thinking.level: expected \"low\" | \"medium\" | \"high\", got \"{other}\""
        ))),
    }
}

fn parse_thinking_budget(raw: Option<&VmValue>) -> Result<Option<u32>, VmError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if matches!(raw, VmValue::Nil) {
        return Ok(None);
    }
    let Some(value) = raw.as_int() else {
        return Err(thinking_error(
            "thinking.budget_tokens: expected a non-negative int",
        ));
    };
    u32::try_from(value)
        .map(Some)
        .map_err(|_| thinking_error("thinking.budget_tokens: expected a non-negative int"))
}

/// Parse the script-facing `thinking` option into a provider-agnostic shape.
///
/// New shape:
///   `{mode: "enabled", budget_tokens: 8000}`
///   `{mode: "adaptive"}`
///   `{mode: "effort", level: "high"}`
///
/// Legacy compatibility:
///   `true` => enabled with provider defaults
///   `{budget_tokens: N}` => enabled with a budget
///   `{enabled: false}` / `false` / `nil` => disabled
fn parse_thinking_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<crate::llm::api::ThinkingConfig, VmError> {
    use crate::llm::api::ThinkingConfig;

    let Some(raw) = options.and_then(|o| o.get("thinking")) else {
        return Ok(ThinkingConfig::Disabled);
    };

    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(ThinkingConfig::Disabled),
        VmValue::Bool(true) => Ok(ThinkingConfig::Enabled {
            budget_tokens: None,
        }),
        VmValue::String(s) => match s.as_ref() {
            "disabled" | "off" | "none" => Ok(ThinkingConfig::Disabled),
            "enabled" | "on" | "true" => Ok(ThinkingConfig::Enabled {
                budget_tokens: None,
            }),
            "adaptive" => Ok(ThinkingConfig::Adaptive),
            "low" | "medium" | "high" => Ok(ThinkingConfig::Effort {
                level: parse_reasoning_effort(s.as_ref())?,
            }),
            other => Err(thinking_error(format!(
                "thinking: expected bool, dict, or one of \"enabled\" | \"adaptive\" | \"low\" | \"medium\" | \"high\", got \"{other}\""
            ))),
        },
        VmValue::Dict(d) => {
            if d.get("enabled").is_some_and(|enabled| !enabled.is_truthy()) {
                return Ok(ThinkingConfig::Disabled);
            }

            let mode = d
                .get("mode")
                .and_then(|value| match value {
                    VmValue::String(s) => Some(s.as_ref()),
                    _ => None,
                })
                .unwrap_or("enabled");

            match mode {
                "disabled" | "off" | "none" => Ok(ThinkingConfig::Disabled),
                "enabled" => Ok(ThinkingConfig::Enabled {
                    budget_tokens: parse_thinking_budget(d.get("budget_tokens"))?,
                }),
                "adaptive" => Ok(ThinkingConfig::Adaptive),
                "effort" => {
                    let level = d
                        .get("level")
                        .and_then(|value| match value {
                            VmValue::String(s) => Some(s.as_ref()),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            thinking_error(
                                "thinking.level is required when thinking.mode is \"effort\"",
                            )
                        })?;
                    Ok(ThinkingConfig::Effort {
                        level: parse_reasoning_effort(level)?,
                    })
                }
                other => Err(thinking_error(format!(
                    "thinking.mode: expected \"disabled\" | \"enabled\" | \"adaptive\" | \"effort\", got \"{other}\""
                ))),
            }
        }
        _ if raw.is_truthy() => Ok(ThinkingConfig::Enabled {
            budget_tokens: None,
        }),
        _ => Ok(ThinkingConfig::Disabled),
    }
}

fn validate_thinking_supported(
    thinking: &crate::llm::api::ThinkingConfig,
    provider: &str,
    model: &str,
    supported_modes: &[String],
) -> Result<(), VmError> {
    use crate::llm::api::ThinkingConfig;

    if thinking.is_disabled() {
        return Ok(());
    }
    let supports = |mode: &str| supported_modes.iter().any(|supported| supported == mode);
    let supported = match thinking {
        ThinkingConfig::Disabled => true,
        // `enabled` remains compatible with Anthropic Opus 4.7+ where
        // providers/anthropic.rs rewrites it to adaptive thinking.
        ThinkingConfig::Enabled { .. } => supports("enabled") || supports("adaptive"),
        ThinkingConfig::Adaptive => supports("adaptive"),
        ThinkingConfig::Effort { .. } => supports("effort"),
    };
    if supported {
        return Ok(());
    }
    let requested = match thinking {
        ThinkingConfig::Disabled => "disabled",
        ThinkingConfig::Enabled { .. } => "enabled",
        ThinkingConfig::Adaptive => "adaptive",
        ThinkingConfig::Effort { .. } => "effort",
    };
    let available = if supported_modes.is_empty() {
        "none".to_string()
    } else {
        supported_modes.join(", ")
    };
    Err(thinking_error(format!(
        "thinking.mode \"{requested}\" is not supported by provider \"{provider}\" model \"{model}\" (supported: {available})"
    )))
}

fn parse_anthropic_beta_features_option(
    options: Option<&BTreeMap<String, VmValue>>,
    thinking: &crate::llm::api::ThinkingConfig,
    provider: &str,
    model: &str,
) -> Result<Vec<String>, VmError> {
    let mut features = Vec::new();
    if let Some(raw) = options.and_then(|o| o.get("anthropic_beta_features")) {
        match raw {
            VmValue::Nil | VmValue::Bool(false) => {}
            VmValue::String(feature) => {
                let feature = feature.as_ref().trim();
                if !feature.is_empty() {
                    validate_anthropic_beta_feature_name(feature)?;
                    crate::llm::api::push_unique_anthropic_beta_feature(&mut features, feature);
                }
            }
            VmValue::List(list) => {
                for item in list.iter() {
                    match item {
                        VmValue::String(feature) => {
                            let feature = feature.as_ref().trim();
                            if !feature.is_empty() {
                                validate_anthropic_beta_feature_name(feature)?;
                                crate::llm::api::push_unique_anthropic_beta_feature(
                                    &mut features,
                                    feature,
                                );
                            }
                        }
                        other => {
                            return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                                format!(
                                    "anthropic_beta_features: expected list<string>, got {}",
                                    other.type_name()
                                ),
                            ))));
                        }
                    }
                }
            }
            other => {
                return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                    format!(
                        "anthropic_beta_features: expected string or list<string>, got {}",
                        other.type_name()
                    ),
                ))));
            }
        }
    }

    if options
        .and_then(|o| o.get("interleaved_thinking"))
        .is_some_and(|value| value.is_truthy())
    {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut features,
            crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
        );
    }

    let caps = crate::llm::capabilities::lookup(provider, model);
    if matches!(
        thinking,
        crate::llm::api::ThinkingConfig::Enabled { .. } | crate::llm::api::ThinkingConfig::Adaptive
    ) && caps.interleaved_thinking_supported
    {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut features,
            crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
        );
    }

    Ok(features)
}

fn validate_anthropic_beta_feature_name(feature: &str) -> Result<(), VmError> {
    if feature
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Ok(());
    }
    Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(format!(
        "anthropic_beta_features: invalid beta feature name `{feature}`; expected ASCII letters, digits, '-' or '_'"
    )))))
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
    use crate::llm::api::{
        ToolSearchConfig, ToolSearchMode, ToolSearchStrategy, ToolSearchVariant,
    };

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
    let strategy_from_short = |s: &str| -> Result<ToolSearchStrategy, VmError> {
        match s {
            "bm25" => Ok(ToolSearchStrategy::Bm25),
            "regex" => Ok(ToolSearchStrategy::Regex),
            "semantic" => Ok(ToolSearchStrategy::Semantic),
            "host" => Ok(ToolSearchStrategy::Host),
            other => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                format!(
                "tool_search.strategy: expected \"bm25\" | \"regex\" | \"semantic\" | \"host\", got \"{other}\""
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
            strategy: None,
            budget_tokens: None,
            name: None,
            include_stub_listing: false,
            deferred_bodies: std::collections::BTreeMap::new(),
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
            let strategy = match d.get("strategy") {
                Some(VmValue::String(s)) => Some(strategy_from_short(s.as_ref())?),
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.strategy: expected a string",
                    ))));
                }
                None => None,
            };
            let budget_tokens = match d.get("budget_tokens") {
                Some(VmValue::Int(n)) => Some(*n),
                Some(VmValue::Nil) | None => None,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.budget_tokens: expected an integer",
                    ))));
                }
            };
            let name = match d.get("name") {
                Some(VmValue::String(s)) => {
                    let s = s.as_ref().trim();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                }
                Some(VmValue::Nil) | None => None,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.name: expected a string",
                    ))));
                }
            };
            let include_stub_listing = match d.get("include_stub_listing") {
                Some(VmValue::Bool(b)) => *b,
                Some(VmValue::Nil) | None => false,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "tool_search.include_stub_listing: expected a bool",
                    ))));
                }
            };
            Ok(Some(ToolSearchConfig {
                variant,
                mode,
                always_loaded,
                strategy,
                budget_tokens,
                name,
                include_stub_listing,
                deferred_bodies: std::collections::BTreeMap::new(),
            }))
        }
        _ => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "tool_search: expected bool, string (\"bm25\"/\"regex\"), or dict \
             ({variant, mode, strategy, always_loaded, budget_tokens, name, include_stub_listing})",
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
        "openai" | "huggingface" | "local" => {
            if opts.top_k.is_some() {
                warn("top_k");
            }
            if opts.cache {
                warn("cache");
            }
        }
        "openrouter" => {
            if opts.top_k.is_some() {
                warn("top_k");
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

#[cfg(test)]
mod output_format_tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn parses_explicit_json_schema_output_format() {
        let mut fmt = BTreeMap::new();
        fmt.insert("kind".to_string(), VmValue::String(Rc::from("json_schema")));
        fmt.insert(
            "schema".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "type".to_string(),
                VmValue::String(Rc::from("object")),
            )]))),
        );
        fmt.insert("strict".to_string(), VmValue::Bool(false));
        let options = BTreeMap::from([("output_format".to_string(), VmValue::Dict(Rc::new(fmt)))]);

        let parsed = parse_output_format_option(Some(&options), None, None).expect("output_format");

        assert_eq!(
            parsed,
            crate::llm::api::OutputFormat::JsonSchema {
                schema: serde_json::json!({"type": "object"}),
                strict: false,
            }
        );
    }

    #[test]
    fn legacy_response_format_and_json_schema_map_to_typed_output_format() {
        let schema = serde_json::json!({"type": "object"});

        let parsed =
            parse_output_format_option(Some(&BTreeMap::new()), Some("json"), Some(&schema))
                .expect("legacy output format");

        assert_eq!(
            parsed,
            crate::llm::api::OutputFormat::JsonSchema {
                schema,
                strict: true,
            }
        );
    }

    #[test]
    fn rejects_json_schema_when_capability_is_absent() {
        crate::llm::capabilities::clear_user_overrides();
        let err = validate_output_format_supported(
            &crate::llm::api::OutputFormat::JsonSchema {
                schema: serde_json::json!({"type": "object"}),
                strict: true,
            },
            "custom-provider",
            "custom-model",
        )
        .expect_err("unsupported structured output should fail");

        assert!(err
            .to_string()
            .contains("cannot enforce strict json_schema output"));
    }

    #[test]
    fn accepts_json_schema_when_capability_declares_strategy() {
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.custom-provider]]
model_match = "*"
structured_output = "format_kw"
"#,
        )
        .expect("capability override");

        validate_output_format_supported(
            &crate::llm::api::OutputFormat::JsonSchema {
                schema: serde_json::json!({"type": "object"}),
                strict: true,
            },
            "custom-provider",
            "custom-model",
        )
        .expect("supported structured output");
        crate::llm::capabilities::clear_user_overrides();
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::llm_config::{AliasDef, AuthEnv, ProviderDef, ProvidersConfig, TierRule};
    use std::rc::Rc;

    fn install_test_routes() {
        let mut overlay = ProvidersConfig::default();
        overlay.providers.insert(
            "cheap".to_string(),
            ProviderDef {
                base_url: "https://cheap.example/v1".to_string(),
                auth_style: "none".to_string(),
                auth_env: AuthEnv::None,
                chat_endpoint: "/chat/completions".to_string(),
                cost_per_1k_in: Some(0.0),
                cost_per_1k_out: Some(0.0),
                latency_p50_ms: Some(2200),
                ..Default::default()
            },
        );
        overlay.providers.insert(
            "fast".to_string(),
            ProviderDef {
                base_url: "https://fast.example/v1".to_string(),
                auth_style: "none".to_string(),
                auth_env: AuthEnv::None,
                chat_endpoint: "/chat/completions".to_string(),
                cost_per_1k_in: Some(0.01),
                cost_per_1k_out: Some(0.02),
                latency_p50_ms: Some(250),
                ..Default::default()
            },
        );
        overlay.aliases.insert(
            "cheap-mid".to_string(),
            AliasDef {
                id: "cheap-mid-model".to_string(),
                provider: "cheap".to_string(),
                tool_format: None,
            },
        );
        overlay.aliases.insert(
            "fast-mid".to_string(),
            AliasDef {
                id: "fast-mid-model".to_string(),
                provider: "fast".to_string(),
                tool_format: None,
            },
        );
        overlay.tier_rules.push(TierRule {
            exact: Some("cheap-mid-model".to_string()),
            pattern: None,
            contains: None,
            tier: "mid".to_string(),
        });
        overlay.tier_rules.push(TierRule {
            exact: Some("fast-mid-model".to_string()),
            pattern: None,
            contains: None,
            tier: "mid".to_string(),
        });
        crate::llm_config::set_user_overrides(Some(overlay));
        super::super::reset_provider_key_cache();
    }

    fn extract_with_policy(policy: &str) -> crate::llm::api::LlmCallOptions {
        let mut options = BTreeMap::new();
        options.insert(
            "route_policy".to_string(),
            VmValue::String(Rc::from(policy.to_string())),
        );
        options.insert(
            "fallback_chain".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from("fast".to_string()))])),
        );
        extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ])
        .expect("options")
    }

    #[test]
    fn cheapest_over_quality_selects_lowest_cost_available_candidate() {
        install_test_routes();
        let opts = extract_with_policy("cheapest_over_quality(mid)");
        assert_eq!(opts.provider, "cheap");
        assert_eq!(opts.model, "cheap-mid-model");
        assert_eq!(opts.fallback_chain, vec!["fast".to_string()]);
        let decision = opts.routing_decision.expect("routing decision");
        assert!(decision.alternatives.iter().any(|alt| alt.selected));
        assert!(decision
            .alternatives
            .iter()
            .any(|alt| alt.provider == "fast"));
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn fastest_over_quality_selects_lowest_latency_available_candidate() {
        install_test_routes();
        let opts = extract_with_policy("fastest_over_quality(mid)");
        assert_eq!(opts.provider, "fast");
        assert_eq!(opts.model, "fast-mid-model");
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn preference_list_cheapest_first_sets_route_fallbacks() {
        install_test_routes();
        let mut policy = BTreeMap::new();
        policy.insert(
            "mode".to_string(),
            VmValue::String(Rc::from("preference_list".to_string())),
        );
        policy.insert(
            "strategy".to_string(),
            VmValue::String(Rc::from("cheapest_first".to_string())),
        );
        policy.insert(
            "prefer".to_string(),
            VmValue::List(Rc::new(vec![
                VmValue::String(Rc::from("fast-mid")),
                VmValue::String(Rc::from("cheap-mid")),
            ])),
        );
        let mut options = BTreeMap::new();
        options.insert("route_policy".to_string(), VmValue::Dict(Rc::new(policy)));
        let opts = extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ])
        .expect("options");

        assert_eq!(opts.provider, "cheap");
        assert_eq!(opts.model, "cheap-mid-model");
        assert_eq!(opts.route_fallbacks.len(), 1);
        assert_eq!(opts.route_fallbacks[0].provider, "fast");
        assert_eq!(opts.route_fallbacks[0].model, "fast-mid-model");
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn always_policy_accepts_provider_model_selector() {
        install_test_routes();
        let opts = extract_with_policy("always(fast:fast-mid-model)");
        assert_eq!(opts.provider, "fast");
        assert_eq!(opts.model, "fast-mid-model");
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn thinking_dict_enabled_false_disables_thinking() {
        let mut options = BTreeMap::new();
        options.insert(
            "provider".to_string(),
            VmValue::String(Rc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(Rc::from("gpt-5.4".to_string())),
        );
        options.insert(
            "thinking".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "enabled".to_string(),
                VmValue::Bool(false),
            )]))),
        );
        let opts = extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ])
        .expect("options");
        assert!(opts.thinking.is_disabled());
    }

    #[test]
    fn thinking_dict_enabled_budget_parses_typed_config() {
        let mut options = BTreeMap::new();
        options.insert(
            "provider".to_string(),
            VmValue::String(Rc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(Rc::from("claude-opus-4-6".to_string())),
        );
        options.insert(
            "thinking".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "mode".to_string(),
                    VmValue::String(Rc::from("enabled".to_string())),
                ),
                ("budget_tokens".to_string(), VmValue::Int(8000)),
            ]))),
        );
        let opts = extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ])
        .expect("options");
        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Enabled {
                budget_tokens: Some(8000)
            }
        );
        assert_eq!(
            opts.anthropic_beta_features,
            vec![crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA]
        );
    }

    #[test]
    fn anthropic_beta_features_parse_and_dedupe_with_interleaved_flag() {
        let mut options = BTreeMap::new();
        options.insert(
            "provider".to_string(),
            VmValue::String(Rc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(Rc::from("claude-opus-4-6".to_string())),
        );
        options.insert(
            "anthropic_beta_features".to_string(),
            VmValue::List(Rc::new(vec![
                VmValue::String(Rc::from("fine-grained-tool-streaming-2025-05-14")),
                VmValue::String(Rc::from(
                    crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
                )),
            ])),
        );
        options.insert("interleaved_thinking".to_string(), VmValue::Bool(true));

        let opts = extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ])
        .expect("options");
        assert_eq!(
            opts.anthropic_beta_features,
            vec![
                "fine-grained-tool-streaming-2025-05-14".to_string(),
                crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA.to_string(),
            ]
        );
    }

    #[test]
    fn anthropic_beta_features_reject_invalid_header_names() {
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(Rc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(Rc::from("claude-opus-4-6".to_string())),
            ),
            (
                "anthropic_beta_features".to_string(),
                VmValue::String(Rc::from("bad\r\nheader".to_string())),
            ),
        ]);

        let err = match extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ]) {
            Ok(_) => panic!("invalid beta feature should fail before transport"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("invalid beta feature name `bad"));
    }

    #[test]
    fn thinking_effort_parses_typed_level() {
        let mut options = BTreeMap::new();
        options.insert(
            "provider".to_string(),
            VmValue::String(Rc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(Rc::from("o3".to_string())),
        );
        options.insert(
            "thinking".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "mode".to_string(),
                    VmValue::String(Rc::from("effort".to_string())),
                ),
                (
                    "level".to_string(),
                    VmValue::String(Rc::from("high".to_string())),
                ),
            ]))),
        );
        let opts = extract_llm_options(&[
            VmValue::String(Rc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(Rc::new(options)),
        ])
        .expect("options");
        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::High
            }
        );
    }

    #[test]
    fn image_content_sets_vision_and_requires_capability() {
        let image_block = VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("image"))),
            (
                "base64".to_string(),
                VmValue::String(Rc::from("iVBORw0KGgo=")),
            ),
            (
                "media_type".to_string(),
                VmValue::String(Rc::from("image/png")),
            ),
        ])));
        let message = VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("user"))),
            (
                "content".to_string(),
                VmValue::List(Rc::new(vec![image_block.clone()])),
            ),
        ])));
        let options = VmValue::Dict(Rc::new(BTreeMap::from([
            ("provider".to_string(), VmValue::String(Rc::from("mock"))),
            ("model".to_string(), VmValue::String(Rc::from("gpt-4o"))),
            (
                "messages".to_string(),
                VmValue::List(Rc::new(vec![message.clone()])),
            ),
        ])));
        let opts =
            extract_llm_options(&[VmValue::String(Rc::from("")), VmValue::Nil, options]).unwrap();
        assert!(opts.vision);

        let bad_options = VmValue::Dict(Rc::new(BTreeMap::from([
            ("provider".to_string(), VmValue::String(Rc::from("mock"))),
            (
                "model".to_string(),
                VmValue::String(Rc::from("gpt-3.5-turbo")),
            ),
            (
                "messages".to_string(),
                VmValue::List(Rc::new(vec![message])),
            ),
        ])));
        let err = extract_llm_options(&[VmValue::String(Rc::from("")), VmValue::Nil, bad_options])
            .err()
            .expect("non-vision model should reject image content");
        assert!(err.to_string().contains("vision_supported"));

        let url_image = VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("image"))),
            (
                "url".to_string(),
                VmValue::String(Rc::from("https://example.com/image.png")),
            ),
            (
                "media_type".to_string(),
                VmValue::String(Rc::from("image/png")),
            ),
        ])));
        let url_message = VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("user"))),
            (
                "content".to_string(),
                VmValue::List(Rc::new(vec![url_image])),
            ),
        ])));
        let ollama_options = VmValue::Dict(Rc::new(BTreeMap::from([
            ("provider".to_string(), VmValue::String(Rc::from("ollama"))),
            (
                "model".to_string(),
                VmValue::String(Rc::from("llava:latest")),
            ),
            (
                "messages".to_string(),
                VmValue::List(Rc::new(vec![url_message])),
            ),
        ])));
        let err =
            extract_llm_options(&[VmValue::String(Rc::from("")), VmValue::Nil, ollama_options])
                .err()
                .expect("ollama should reject url image content");
        assert!(err.to_string().contains("requires image base64"));
    }
}
