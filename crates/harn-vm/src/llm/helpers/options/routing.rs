use super::*;

const DEFAULT_EQUIVALENT_FAILOVER_MAX_ROUTES: usize = 3;

pub(super) fn quality_rank(tier: &str) -> i32 {
    match tier.to_ascii_lowercase().as_str() {
        "small" => 0,
        "mid" | "medium" => 1,
        "frontier" | "large" => 2,
        _ => 1,
    }
}

pub(super) fn route_target_from_short(
    target: &str,
) -> Result<(String, String), crate::value::VmError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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

pub(super) fn parse_route_policy_text(
    text: &str,
) -> Result<crate::llm::api::LlmRoutePolicy, VmError> {
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
    Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "route_policy: expected manual, always(id), cheapest_over_quality(t), or fastest_over_quality(t), got {text:?}"
    )))))
}

pub(super) fn vm_string_list(value: &VmValue) -> Vec<String> {
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

pub(super) fn parse_route_policy_option(
    options: Option<&crate::value::DictMap>,
) -> Result<crate::llm::api::LlmRoutePolicy, VmError> {
    use crate::llm::api::LlmRoutePolicy;
    let Some(raw) = options.and_then(|o| o.get("route_policy")) else {
        return Ok(LlmRoutePolicy::Manual);
    };
    match raw {
        VmValue::Nil => Ok(LlmRoutePolicy::Manual),
        VmValue::Bool(false) => Ok(LlmRoutePolicy::Manual),
        VmValue::String(text) => parse_route_policy_text(text),
        VmValue::Dict(d) => {
            // Canonical dict grammar: {mode, target?, targets?, strategy?}. Any
            // other key is a hard error rather than a silent drop.
            for (key, _) in d.iter() {
                let key: &str = key.as_ref();
                if !matches!(key, "mode" | "target" | "targets" | "strategy") {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        format!(
                            "route_policy: unknown key `{key}` — the dict form is \
                             {{mode, target?, targets?, strategy?}}"
                        ),
                    ))));
                }
            }
            let mode = d
                .get("mode")
                .map(|value| value.display())
                .unwrap_or_else(|| "manual".to_string());
            let target = d
                .get("target")
                .map(|value| value.display())
                .unwrap_or_default();
            match mode.as_str() {
                "manual" => Ok(LlmRoutePolicy::Manual),
                "always" => Ok(LlmRoutePolicy::Always(target)),
                "cheapest_over_quality" => Ok(LlmRoutePolicy::CheapestOverQuality(target)),
                "fastest_over_quality" => Ok(LlmRoutePolicy::FastestOverQuality(target)),
                "preference_list" => {
                    let targets = d.get("targets").map(vm_string_list).unwrap_or_default();
                    if targets.is_empty() {
                        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                            "route_policy.targets: expected at least one model/provider target",
                        ))));
                    }
                    let strategy = d
                        .get("strategy")
                        .map(|value| value.display())
                        .unwrap_or_else(|| "prefer_order".to_string());
                    Ok(LlmRoutePolicy::PreferenceList { targets, strategy })
                }
                other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                        "route_policy.mode: unsupported value `{other}` — expected \
                         manual, always, cheapest_over_quality, fastest_over_quality, \
                         or preference_list"
                    ),
                )))),
            }
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "route_policy: expected string or dict",
        )))),
    }
}

pub(super) fn parse_fallback_chain_option(options: Option<&crate::value::DictMap>) -> Vec<String> {
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

pub(super) fn parse_equivalent_failover_option(
    options: Option<&crate::value::DictMap>,
    provider: &str,
    model: &str,
    explicit_routing_policy: bool,
    requirements: crate::llm_config::EquivalentModelRequirements,
) -> Result<Option<std::sync::Arc<crate::llm::routing::RoutingPolicyConfig>>, VmError> {
    let Some(raw) = options.and_then(|o| o.get("equivalent_failover")) else {
        return Ok(None);
    };

    let mut enabled = true;
    let mut max_routes = DEFAULT_EQUIVALENT_FAILOVER_MAX_ROUTES;
    let mut on_no_dispatch = false;
    match raw {
        VmValue::Nil | VmValue::Bool(false) => return Ok(None),
        VmValue::Bool(true) => {}
        VmValue::Dict(dict) => {
            if let Some(value) = dict.get("enabled") {
                enabled = match value {
                    VmValue::Nil => true,
                    VmValue::Bool(value) => *value,
                    other => {
                        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                            format!(
                                "equivalent_failover.enabled: expected bool, got {}",
                                other.type_name()
                            ),
                        ))));
                    }
                };
            }
            if let Some(value) = dict.get("on_no_dispatch") {
                on_no_dispatch = match value {
                    VmValue::Nil => false,
                    VmValue::Bool(value) => *value,
                    other => {
                        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                            format!(
                                "equivalent_failover.on_no_dispatch: expected bool, got {}",
                                other.type_name()
                            ),
                        ))));
                    }
                };
            }
            if let Some(value) = dict.get("max_routes") {
                let raw_max = value.as_int().ok_or_else(|| {
                    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "equivalent_failover.max_routes: expected a positive integer",
                    )))
                })?;
                if raw_max < 1 {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "equivalent_failover.max_routes: expected a positive integer",
                    ))));
                }
                max_routes = raw_max as usize;
            }
        }
        other => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "equivalent_failover: expected bool or dict, got {}",
                    other.type_name()
                ),
            ))));
        }
    }

    if !enabled {
        return Ok(None);
    }
    if explicit_routing_policy || equivalent_failover_has_route_owner(options) {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "equivalent_failover cannot be combined with explicit routing policy options",
        ))));
    }

    Ok(crate::llm::routing::build_equivalent_failover_policy(
        provider,
        model,
        max_routes,
        on_no_dispatch,
        requirements,
    ))
}

fn equivalent_failover_has_route_owner(options: Option<&crate::value::DictMap>) -> bool {
    let Some(options) = options else {
        return false;
    };
    [
        "routing",
        "models",
        "ladder",
        "route_policy",
        "fallback_chain",
    ]
    .iter()
    .any(|key| route_owner_option_present(options, key))
}

fn route_owner_option_present(options: &crate::value::DictMap, key: &str) -> bool {
    options
        .get(key)
        .is_some_and(|value| !matches!(value, VmValue::Nil | VmValue::Bool(false)))
}

pub(super) fn equivalent_failover_requirements_for_options(
    opts: &crate::llm::api::LlmCallOptions,
) -> crate::llm_config::EquivalentModelRequirements {
    let native_tools = opts
        .native_tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
        || !opts.provider_tools.is_empty();
    let text_tool_wire_format = opts.tools.is_some() && !native_tools;
    let thinking = opts.thinking.is_enabled();
    let reasoning_effort = matches!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::Minimal
                | crate::llm::api::ReasoningEffort::Low
                | crate::llm::api::ReasoningEffort::Medium
                | crate::llm::api::ReasoningEffort::High
                | crate::llm::api::ReasoningEffort::XHigh
                | crate::llm::api::ReasoningEffort::Max,
        }
    );
    let structured_output = opts.output_format.is_structured() || opts.output_schema.is_some();
    let mut provider_tool_types = equivalent_provider_tool_types_for_options(opts);

    provider_tool_types.sort();
    provider_tool_types.dedup();
    crate::llm_config::EquivalentModelRequirements {
        context_tokens: Some(crate::llm::cost::project_llm_call_context_tokens(opts)),
        native_tools,
        text_tool_wire_format,
        provider_tool_types,
        vision: opts.vision,
        url_images: crate::llm::content::messages_contain_url_images(&opts.messages)
            .unwrap_or(false),
        audio: crate::llm::content::messages_contain_audio(&opts.messages).unwrap_or(false),
        pdf: crate::llm::content::messages_contain_pdf(&opts.messages).unwrap_or(false),
        video: crate::llm::content::messages_contain_videos(&opts.messages).unwrap_or(false),
        files_api: crate::llm::content::messages_contain_file_ids(&opts.messages).unwrap_or(false),
        thinking,
        reasoning_effort,
        structured_output,
        structured_output_mode: None,
    }
}

fn equivalent_provider_tool_types_for_options(
    opts: &crate::llm::api::LlmCallOptions,
) -> Vec<String> {
    let mut kinds = Vec::new();
    if let Some(native_tools) = opts.native_tools.as_ref() {
        for tool in native_tools {
            if provider_tool_type(tool)
                .as_deref()
                .is_some_and(|kind| kind == "computer_use")
            {
                kinds.push("computer_use".to_string());
            }
        }
    }
    for tool in &opts.provider_tools {
        kinds.push(
            provider_tool_type(tool).unwrap_or_else(|| "__unknown_provider_tool__".to_string()),
        );
    }
    kinds
}

fn provider_tool_type(tool: &serde_json::Value) -> Option<String> {
    let raw = tool
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if raw.starts_with("computer") {
        Some("computer_use".to_string())
    } else {
        Some(raw.to_string())
    }
}

pub(super) fn route_alternative(
    provider: String,
    model: String,
    selected: bool,
    reason: String,
) -> crate::llm::api::LlmRouteAlternative {
    let quality_tier = crate::llm_config::model_tier(&model);
    let pricing = crate::llm::cost::pricing_per_1k_for(&provider, &model);
    crate::llm::api::LlmRouteAlternative {
        available: crate::llm::provider_auth_status(&provider).available,
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

pub(super) fn resolve_route_policy(
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
