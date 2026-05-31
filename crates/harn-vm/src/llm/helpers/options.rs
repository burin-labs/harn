//! LLM call option extraction — parses the `(prompt, system, options)`
//! argument shape every high-level builtin accepts into the canonical
//! `LlmCallOptions` struct, including provider-specific warnings.

use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::{
    emit_reminder_lifecycle_event, opt_bool, opt_float, opt_int, opt_str, provider_key_available,
    reminder_from_event, resolve_api_key, vm_messages_to_json, vm_resolve_model,
    vm_resolve_provider, vm_value_dict_to_json, vm_value_to_json, ReminderRoleHint, SystemReminder,
    REMINDER_DROPPED_EVENT_KIND, SYSTEM_REMINDER_EVENT_KIND,
};

pub(crate) fn extract_json(text: &str) -> String {
    crate::stdlib::json::extract_json_from_text(text)
}

/// Resolve the wall-clock timeout from the (`timeout`, `timeout_ms`) pair.
///
/// `timeout` (canonical, seconds) wins when explicitly set. Otherwise
/// `timeout_ms` is accepted for symmetry with the broader option surface
/// (`with_timeout`, HTTP, command exec) and rounded UP to the nearest
/// whole second — the underlying HTTP transports all consume
/// `Duration::from_secs(u64)`. Sub-second budgets must be enforced at
/// the caller (e.g. the wall-clock post-check in `std/llm/tool_binder`).
pub(crate) fn resolve_timeout_secs(timeout: Option<i64>, timeout_ms: Option<i64>) -> Option<u64> {
    if let Some(seconds) = timeout {
        return Some(seconds.max(0) as u64);
    }
    timeout_ms.map(|ms| {
        if ms <= 0 {
            0
        } else {
            (ms as u64).div_ceil(1000)
        }
    })
}

#[cfg(test)]
mod resolve_timeout_secs_tests {
    use super::resolve_timeout_secs;

    #[test]
    fn explicit_timeout_wins_over_timeout_ms() {
        assert_eq!(resolve_timeout_secs(Some(5), Some(100)), Some(5));
    }

    #[test]
    fn timeout_ms_rounds_up_to_seconds() {
        assert_eq!(resolve_timeout_secs(None, Some(1)), Some(1));
        assert_eq!(resolve_timeout_secs(None, Some(100)), Some(1));
        assert_eq!(resolve_timeout_secs(None, Some(1000)), Some(1));
        assert_eq!(resolve_timeout_secs(None, Some(1001)), Some(2));
        assert_eq!(resolve_timeout_secs(None, Some(5000)), Some(5));
    }

    #[test]
    fn non_positive_clamps_to_zero() {
        assert_eq!(resolve_timeout_secs(None, Some(0)), Some(0));
        assert_eq!(resolve_timeout_secs(None, Some(-1)), Some(0));
        assert_eq!(resolve_timeout_secs(Some(-1), None), Some(0));
    }

    #[test]
    fn returns_none_when_neither_set() {
        assert_eq!(resolve_timeout_secs(None, None), None);
    }
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
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
    Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
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
                        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
                other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!("route_policy.mode: unsupported value {other:?}"),
                )))),
            }
        }
        _ => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
/// reads linearly; the `Client` variant leaves the fallback to the Harn
/// agent loop, and the `Native` variant feeds provider-native injection.
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
/// the native tool-search meta-tool.
fn classify_native_shape(
    provider: &str,
    model: &str,
) -> crate::llm::provider::NativeToolSearchShape {
    crate::llm::provider::provider_native_tool_search_shape(provider, model)
}

fn parse_api_mode_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<crate::llm::api::LlmApiMode, VmError> {
    let Some(raw) = options.and_then(|o| o.get("api_mode").or_else(|| o.get("api"))) else {
        return Ok(crate::llm::api::LlmApiMode::ChatCompletions);
    };
    match raw {
        VmValue::Nil => Ok(crate::llm::api::LlmApiMode::ChatCompletions),
        VmValue::String(value) => {
            let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
            match normalized.as_str() {
                "chat" | "chat_completions" | "chat_completion" | "completions" => {
                    Ok(crate::llm::api::LlmApiMode::ChatCompletions)
                }
                "responses" | "response" => Ok(crate::llm::api::LlmApiMode::Responses),
                other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                        "api_mode: expected \"chat_completions\" or \"responses\", got \"{other}\""
                    ),
                )))),
            }
        }
        other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("api_mode: expected a string, got {}", other.type_name()),
        )))),
    }
}

fn enforce_responses_provider_gate(mode: crate::llm::api::LlmApiMode, provider: &str) -> bool {
    mode == crate::llm::api::LlmApiMode::Responses && provider != "openai" && provider != "mock"
}

fn parse_provider_tools_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Vec<serde_json::Value>, VmError> {
    let Some(raw) = options.and_then(|o| o.get("provider_tools").or_else(|| o.get("hosted_tools")))
    else {
        return Ok(Vec::new());
    };
    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(Vec::new()),
        VmValue::Dict(_) => Ok(vec![vm_value_to_json(raw)]),
        VmValue::List(list) => list
            .iter()
            .map(|value| match value {
                VmValue::String(kind) => Ok(serde_json::json!({"type": kind.as_ref()})),
                VmValue::Dict(_) => Ok(vm_value_to_json(value)),
                other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                        "provider_tools: expected each entry to be a dict or string, got {}",
                        other.type_name()
                    ),
                )))),
            })
            .collect(),
        other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!(
                "provider_tools: expected a list or dict, got {}",
                other.type_name()
            ),
        )))),
    }
}

fn opt_bool_field(
    options: Option<&BTreeMap<String, VmValue>>,
    key: &str,
) -> Result<Option<bool>, VmError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("{key}: expected a bool, got {}", other.type_name()),
        )))),
    }
}

fn opt_responses_store_field(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Option<bool>, VmError> {
    if let Some(value) = opt_bool_field(options, "response_store")? {
        return Ok(Some(value));
    }
    if let Some(value) = opt_bool_field(options, "responses_store")? {
        return Ok(Some(value));
    }
    match options.and_then(|o| o.get("store")) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        _ => Ok(None),
    }
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
                VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                    "{field}: expected a JSON Schema object"
                ))))
            }),
    }
}

fn output_format_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

fn unsupported_option_error(option: &str, provider: &str, model: &str) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
        "option `{option}` is not supported by `{model}` (provider `{provider}`). See `harn providers matrix` for compatibility."
    ))))
}

fn option_is_enabled(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> bool {
    options
        .and_then(|o| o.get(key))
        .is_some_and(|value| value.is_truthy())
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
    caps: &crate::llm::capabilities::Capabilities,
) -> Result<(), VmError> {
    use crate::llm::api::OutputFormat;

    match output_format {
        OutputFormat::Text => Ok(()),
        _ if provider == "mock" => Ok(()),
        OutputFormat::JsonObject => {
            if caps.structured_output.is_some() {
                Ok(())
            } else {
                Err(unsupported_option_error("output_format", provider, model))
            }
        }
        OutputFormat::JsonSchema { .. } => {
            match caps.structured_output.as_deref() {
                Some("native" | "tool_use" | "format_kw") => Ok(()),
                Some(other) => Err(output_format_error(format!(
                    "output_format: provider \"{provider}\" model \"{model}\" declares unsupported structured_output strategy \"{other}\""
                ))),
                None => Err(unsupported_option_error("output_format", provider, model)),
            }
        }
    }
}

/// Layer the active step's defaults onto the call options dict before
/// model/provider resolution and budget parsing run. The model override
/// is a no-op when the user explicitly passed `model:`. The budget
/// merge is non-destructive: only fields the call site didn't already
/// set are filled in, so a tighter explicit ceiling always wins.
fn apply_active_step_defaults(options: &mut Option<BTreeMap<String, VmValue>>) {
    let user_supplied_model = options
        .as_ref()
        .map(|o| o.contains_key("model"))
        .unwrap_or(false);
    let step_default = if user_supplied_model {
        None
    } else {
        crate::step_runtime::active_step_model_default()
    };
    let step_budget = crate::step_runtime::with_active_step(|step| step.definition.clone())
        .map(|definition| (definition.max_tokens, definition.max_usd));
    if step_default.is_none() && step_budget.is_none() {
        return;
    }
    let opts = options.get_or_insert_with(BTreeMap::new);
    if let Some(model_name) = step_default {
        opts.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from(model_name)),
        );
    }
    if let Some((max_tokens, max_usd)) = step_budget {
        if max_tokens.is_some() || max_usd.is_some() {
            // Project the step budget onto `llm_call`'s preflight
            // budget envelope so the existing accumulator + projection
            // machinery short-circuits a call that would obviously
            // exceed the step's ceiling.
            let mut step_budget_dict: BTreeMap<String, VmValue> = match opts.get("budget") {
                Some(VmValue::Dict(existing)) => (**existing).clone(),
                _ => BTreeMap::new(),
            };
            if let Some(max_tokens) = max_tokens {
                step_budget_dict
                    .entry("max_output_tokens".to_string())
                    .or_insert_with(|| VmValue::Int(max_tokens as i64));
            }
            if let Some(max_usd) = max_usd {
                step_budget_dict
                    .entry("max_cost_usd".to_string())
                    .or_insert_with(|| VmValue::Float(max_usd));
            }
            opts.insert(
                "budget".to_string(),
                VmValue::Dict(std::sync::Arc::new(step_budget_dict)),
            );
        }
    }
}

fn toml_value_to_vm_value(value: &toml::Value) -> VmValue {
    match value {
        toml::Value::String(s) => VmValue::String(std::sync::Arc::from(s.as_str())),
        toml::Value::Integer(i) => VmValue::Int(*i),
        toml::Value::Float(f) => VmValue::Float(*f),
        toml::Value::Boolean(b) => VmValue::Bool(*b),
        toml::Value::Datetime(dt) => VmValue::String(std::sync::Arc::from(dt.to_string())),
        toml::Value::Array(items) => VmValue::List(std::sync::Arc::new(
            items.iter().map(toml_value_to_vm_value).collect(),
        )),
        toml::Value::Table(table) => VmValue::Dict(std::sync::Arc::new(
            table
                .iter()
                .map(|(key, value)| (key.clone(), toml_value_to_vm_value(value)))
                .collect(),
        )),
    }
}

fn model_role_option(options: &Option<BTreeMap<String, VmValue>>) -> Option<String> {
    options
        .as_ref()
        .and_then(|opts| opts.get("model_role").or_else(|| opts.get("role")))
        .filter(|value| !matches!(value, VmValue::Nil | VmValue::Bool(false)))
        .map(VmValue::display)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn apply_model_role_defaults(options: &mut Option<BTreeMap<String, VmValue>>) {
    let Some(role) = model_role_option(options) else {
        return;
    };
    let defaults = crate::llm_config::model_role_defaults(&role);
    if defaults.is_empty() {
        return;
    }
    let opts = options.get_or_insert_with(BTreeMap::new);
    for (key, value) in defaults {
        if key == "model_role" || key == "role" {
            continue;
        }
        opts.entry(key)
            .or_insert_with(|| toml_value_to_vm_value(&value));
    }
}

#[derive(Clone, Copy)]
enum SystemPromptPosition {
    Before,
    After,
}

fn system_prompt_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

fn system_prompt_position(
    value: Option<&VmValue>,
    source: &str,
    fallback: SystemPromptPosition,
) -> Result<SystemPromptPosition, VmError> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    match value {
        VmValue::Nil => Ok(fallback),
        VmValue::String(raw) => match raw.as_ref() {
            "before" | "prepend" | "prefix" | "start" => Ok(SystemPromptPosition::Before),
            "after" | "append" | "suffix" | "end" => Ok(SystemPromptPosition::After),
            other => Err(system_prompt_error(format!(
                "{source}.position: expected \"before\" or \"after\", got \"{other}\""
            ))),
        },
        other => Err(system_prompt_error(format!(
            "{source}.position: expected a string, got {}",
            other.type_name()
        ))),
    }
}

fn enabled_system_prompt_part(part: &BTreeMap<String, VmValue>) -> bool {
    !matches!(
        part.get("enabled"),
        Some(VmValue::Bool(false) | VmValue::Nil)
    )
}

fn system_prompt_part_content(part: &BTreeMap<String, VmValue>) -> Option<String> {
    part.get("content")
        .or_else(|| part.get("text"))
        .or_else(|| part.get("prompt"))
        .map(VmValue::display)
}

fn render_system_prompt_part(content: String, part: &BTreeMap<String, VmValue>) -> String {
    let title = part
        .get("label")
        .or_else(|| part.get("title"))
        .or_else(|| part.get("name"))
        .map(VmValue::display)
        .unwrap_or_default();
    let title = title.trim();
    let content = content.trim();
    if title.is_empty() {
        content.to_string()
    } else {
        format!("## {title}\n{content}")
    }
}

/// Expand a host-provided system-prompt option (`system_preamble`,
/// `system_prompt_parts`, …) into [`crate::llm::prompt::PromptFragment`]s,
/// faithfully mirroring the legacy string / list / dict shapes
/// (`{content|text|prompt, position, parts, enabled, label}`). The resulting
/// fragments are reduced by [`crate::llm::prompt::assemble`].
fn append_host_fragments(
    out: &mut Vec<crate::llm::prompt::PromptFragment>,
    value: Option<&VmValue>,
    source: &str,
    forced_position: SystemPromptPosition,
) -> Result<(), VmError> {
    use crate::llm::prompt::PromptFragment;
    let Some(value) = value else {
        return Ok(());
    };
    match value {
        VmValue::Nil | VmValue::Bool(false) => Ok(()),
        VmValue::String(text) => {
            out.push(PromptFragment::new(
                format!("host:{source}"),
                format!("host:{source}"),
                fragment_bucket(forced_position),
                text.to_string(),
            ));
            Ok(())
        }
        VmValue::List(items) => {
            for (index, item) in items.iter().enumerate() {
                append_host_fragments(
                    out,
                    Some(item),
                    &format!("{source}[{index}]"),
                    forced_position,
                )?;
            }
            Ok(())
        }
        VmValue::Dict(part) => {
            if !enabled_system_prompt_part(part) {
                return Ok(());
            }
            let position = system_prompt_position(part.get("position"), source, forced_position)?;
            if let Some(parts) = part.get("parts") {
                return append_host_fragments(out, Some(parts), source, position);
            }
            let content = system_prompt_part_content(part).ok_or_else(|| {
                system_prompt_error(format!(
                    "{source}: system prompt part must include `content`, `text`, `prompt`, or `parts`"
                ))
            })?;
            let rendered = render_system_prompt_part(content, part);
            out.push(PromptFragment::new(
                format!("host:{source}"),
                format!("host:{source}"),
                fragment_bucket(position),
                rendered,
            ));
            Ok(())
        }
        other => Err(system_prompt_error(format!(
            "{source}: expected a string, dict, list, nil, or false; got {}",
            other.type_name()
        ))),
    }
}

fn fragment_bucket(position: SystemPromptPosition) -> crate::llm::prompt::FragmentBucket {
    match position {
        SystemPromptPosition::Before => crate::llm::prompt::FragmentBucket::Before,
        SystemPromptPosition::After => crate::llm::prompt::FragmentBucket::After,
    }
}

fn system_prompt_fingerprint(system: &str) -> String {
    use sha2::Digest as _;

    let digest = sha2::Sha256::digest(system.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

pub(crate) fn system_prompt_metadata(system: &str) -> serde_json::Value {
    let fingerprint = system_prompt_fingerprint(system);
    serde_json::json!({
        "content": system,
        "hash": fingerprint,
        "sha256": fingerprint,
        "bytes": system.len(),
    })
}

pub(crate) fn system_prompt_event_metadata(system: &str) -> serde_json::Value {
    let fingerprint = system_prompt_fingerprint(system);
    serde_json::json!({
        "hash": fingerprint,
        "sha256": fingerprint,
        "bytes": system.len(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderedReminder {
    SystemText(String),
    Message(serde_json::Value),
}

impl RenderedReminder {
    fn rendered_role(&self) -> String {
        match self {
            Self::SystemText(_) => "system".to_string(),
            Self::Message(message) => message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("system")
                .to_string(),
        }
    }
}

fn escape_xml_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn reminder_xml_text(reminder: &SystemReminder) -> String {
    format!(
        "<system-reminder>\n{}\n</system-reminder>",
        escape_xml_text(&reminder.body)
    )
}

fn reminder_plain_text(reminder: &SystemReminder) -> String {
    format!("System reminder:\n{}", reminder.body)
}

fn reminder_system_text(
    caps: &crate::llm::capabilities::Capabilities,
    reminder: &SystemReminder,
) -> String {
    if caps.prefers_xml_scaffolding {
        reminder_xml_text(reminder)
    } else {
        reminder_plain_text(reminder)
    }
}

fn reminder_developer_message(reminder: &SystemReminder) -> RenderedReminder {
    RenderedReminder::Message(serde_json::json!({
        "role": "developer",
        "content": reminder_plain_text(reminder),
    }))
}

fn reminder_user_block_message(
    caps: &crate::llm::capabilities::Capabilities,
    reminder: &SystemReminder,
    cache_control: bool,
) -> RenderedReminder {
    let mut block = serde_json::json!({
        "type": "text",
        "text": reminder_xml_text(reminder),
    });
    if cache_control && caps.prompt_caching {
        block["cache_control"] = serde_json::json!({"type": "ephemeral"});
    }
    RenderedReminder::Message(serde_json::json!({
        "role": "user",
        "content": [block],
    }))
}

pub(crate) fn render_pending_reminders(
    caps: &crate::llm::capabilities::Capabilities,
    reminders: &[SystemReminder],
) -> Vec<RenderedReminder> {
    reminders
        .iter()
        .map(|reminder| {
            if caps.prefers_role_developer {
                return reminder_developer_message(reminder);
            }
            if caps.message_wire_format == "anthropic" {
                return match reminder.role_hint {
                    ReminderRoleHint::UserBlock => {
                        reminder_user_block_message(caps, reminder, false)
                    }
                    ReminderRoleHint::EphemeralCache => {
                        reminder_user_block_message(caps, reminder, true)
                    }
                    ReminderRoleHint::System | ReminderRoleHint::Developer => {
                        RenderedReminder::SystemText(reminder_system_text(caps, reminder))
                    }
                };
            }
            RenderedReminder::SystemText(reminder_system_text(caps, reminder))
        })
        .collect()
}

fn rendered_reminder_lifecycle(
    session_id: Option<&str>,
    turn_number: i64,
    reminders: &[SystemReminder],
    rendered: &[RenderedReminder],
) -> Vec<crate::llm::api::ReminderLifecycleEmission> {
    reminders
        .iter()
        .zip(rendered.iter())
        .map(|(reminder, rendered)| {
            let rendered_role = rendered.rendered_role();
            crate::llm::api::ReminderLifecycleEmission {
                session_id: session_id.map(str::to_string),
                turn_number,
                reminder_id: reminder.id.clone(),
                tags: reminder.tags.clone(),
                body: reminder.body.clone(),
                dedupe_key: reminder.dedupe_key.clone(),
                source: reminder.source.as_str().to_string(),
                role_hint: reminder.role_hint.as_str().to_string(),
                rendered_role,
                ttl_turns: reminder.ttl_turns,
                propagate: reminder.propagate.as_str().to_string(),
                originating_agent_id: reminder.originating_agent_id.clone(),
            }
        })
        .collect()
}

fn emit_dropped_reminder_lifecycle(session_id: &str, reminder_id: String, reason: &str) {
    emit_reminder_lifecycle_event(
        REMINDER_DROPPED_EVENT_KIND,
        serde_json::json!({
            "session_id": session_id,
            "reminder_id": reminder_id,
            "reason": reason,
        }),
    );
}

fn pending_reminders_from_session(session_id: Option<&str>) -> Vec<SystemReminder> {
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return Vec::new();
    };
    let Some(dict) = transcript.as_dict() else {
        return Vec::new();
    };
    let events = dict.get("events").or_else(|| dict.get("messages"));
    let Some(VmValue::List(items)) = events else {
        return Vec::new();
    };
    let mut reminders = Vec::new();
    let mut invalid_count = 0;
    for event in items.iter() {
        if let Some(reminder) = reminder_from_event(event) {
            if reminder.body.trim().is_empty() {
                invalid_count += 1;
                emit_dropped_reminder_lifecycle(session_id, reminder.id, "invalid");
                continue;
            }
            reminders.push(reminder);
            continue;
        }
        let Some(dict) = event.as_dict() else {
            continue;
        };
        if dict.get("kind").map(VmValue::display).as_deref() != Some(SYSTEM_REMINDER_EVENT_KIND) {
            continue;
        }
        invalid_count += 1;
        let reminder_id = dict
            .get("reminder")
            .and_then(VmValue::as_dict)
            .and_then(|reminder| reminder.get("id"))
            .map(VmValue::display)
            .filter(|id| !id.is_empty())
            .or_else(|| {
                dict.get("id")
                    .map(VmValue::display)
                    .filter(|id| !id.is_empty())
            })
            .unwrap_or_else(|| "invalid-reminder".to_string());
        emit_dropped_reminder_lifecycle(session_id, reminder_id, "invalid");
    }
    if invalid_count > 0 {
        crate::agent_sessions::prune_invalid_reminder_events(session_id);
    }
    reminders
}

fn prepend_content_blocks(content: &mut serde_json::Value, mut blocks: Vec<serde_json::Value>) {
    if let serde_json::Value::Array(existing) = content {
        blocks.append(existing);
        *existing = blocks;
        return;
    }
    if let serde_json::Value::String(text) = content {
        blocks.push(serde_json::json!({"type": "text", "text": text.clone()}));
        *content = serde_json::Value::Array(blocks);
        return;
    }
    if content.is_null() {
        *content = serde_json::Value::Array(blocks);
        return;
    }
    blocks.push(std::mem::take(content));
    *content = serde_json::Value::Array(blocks);
}

fn try_prepend_user_reminder(
    messages: &mut [serde_json::Value],
    reminder: &serde_json::Value,
) -> bool {
    if reminder.get("role").and_then(|role| role.as_str()) != Some("user") {
        return false;
    }
    let Some(blocks) = reminder
        .get("content")
        .and_then(|content| content.as_array())
        .cloned()
    else {
        return false;
    };
    let Some(first) = messages.first_mut() else {
        return false;
    };
    let Some(first_obj) = first.as_object_mut() else {
        return false;
    };
    if first_obj.get("role").and_then(|role| role.as_str()) != Some("user") {
        return false;
    }
    let content = first_obj
        .entry("content".to_string())
        .or_insert(serde_json::Value::Null);
    prepend_content_blocks(content, blocks);
    true
}

fn apply_rendered_reminder_messages(
    messages: Vec<serde_json::Value>,
    rendered: &[RenderedReminder],
) -> Vec<serde_json::Value> {
    let mut messages = messages;
    let mut prefix = Vec::new();
    for reminder in rendered {
        let RenderedReminder::Message(message) = reminder else {
            continue;
        };
        if !try_prepend_user_reminder(&mut messages, message) {
            prefix.push(message.clone());
        }
    }
    prefix.extend(messages);
    prefix
}

pub(crate) fn compose_system_prompt(
    system: Option<String>,
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Option<String>, VmError> {
    compose_system_prompt_with_reminders(system, options, &[])
}

fn compose_system_prompt_with_reminders(
    system: Option<String>,
    options: Option<&BTreeMap<String, VmValue>>,
    rendered_reminders: &[RenderedReminder],
) -> Result<Option<String>, VmError> {
    Ok(assemble_system_prompt(system, options, rendered_reminders)?.system)
}

/// Build the system prompt as an ordered list of fragments and reduce them,
/// returning the assembled string together with per-fragment provenance.
///
/// This is the single assembly path: host-provided parts, the primary system
/// text, capability-gated tool guidance, and rendered system reminders all
/// flow through the same [`crate::llm::prompt::assemble`] reducer.
/// [`compose_system_prompt_with_reminders`] is the thin string-only wrapper.
pub(crate) fn assemble_system_prompt(
    system: Option<String>,
    options: Option<&BTreeMap<String, VmValue>>,
    rendered_reminders: &[RenderedReminder],
) -> Result<crate::llm::prompt::AssembledPrompt, VmError> {
    use crate::llm::prompt::{assemble, FragmentBucket, PromptFragment};

    let mut fragments: Vec<PromptFragment> = Vec::new();
    if let Some(options) = options {
        append_host_fragments(
            &mut fragments,
            options.get("system_preamble"),
            "system_preamble",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_prefix"),
            "system_prefix",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_context"),
            "system_context",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_prompt_parts"),
            "system_prompt_parts",
            SystemPromptPosition::Before,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_appendix"),
            "system_appendix",
            SystemPromptPosition::After,
        )?;
        append_host_fragments(
            &mut fragments,
            options.get("system_suffix"),
            "system_suffix",
            SystemPromptPosition::After,
        )?;
    }

    // The agent loop hands us the primary block pre-decomposed into its
    // constituent parts (system text, MCP advisory, active skills, skill
    // catalog, progress nudge, loop/tool contracts) via `_system_fragments`,
    // so each part is individually auditable instead of opaque inside one
    // joined string. When present it fully supersedes the single-string
    // primary path (the `system` arg / `opts.system` is already represented as
    // one of those fragments).
    let decomposed = append_decomposed_primary_fragments(&mut fragments, options)?;
    if !decomposed {
        let primary_system = system
            .filter(|system| !system.trim().is_empty())
            .or_else(|| {
                options
                    .and_then(|options| options.get("system"))
                    .filter(|value| !matches!(value, VmValue::Nil | VmValue::Bool(false)))
                    .map(VmValue::display)
                    .filter(|system| !system.trim().is_empty())
            });
        if let Some(system) = primary_system {
            fragments.push(PromptFragment::new(
                "primary",
                "primary",
                FragmentBucket::Before,
                system,
            ));
        }
    }

    // Capability-gated tool guidance: each active tool that declares a
    // `guidance` string contributes an instruction fragment gated on the
    // tool's own presence. Tool and instruction share one source of truth and
    // cannot drift. Dormant until a tool actually carries `guidance`.
    append_tool_guidance_fragments(&mut fragments, options);

    for reminder in rendered_reminders {
        if let RenderedReminder::SystemText(text) = reminder {
            fragments.push(PromptFragment::new(
                "reminder",
                "reminder",
                FragmentBucket::Before,
                text.clone(),
            ));
        }
    }

    let ctx = assemble_ctx(options);
    Ok(assemble(&fragments, &ctx))
}

/// Names of the tools active for this call, read from the `tools` option
/// (either a list of tool dicts or a `{tools: [...]}` registry).
fn tool_names_from_options(
    options: Option<&BTreeMap<String, VmValue>>,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let Some(list) = options.and_then(|options| tool_entry_list(options.get("tools"))) else {
        return names;
    };
    for entry in list.iter() {
        if let Some(name) = entry
            .as_dict()
            .and_then(|dict| dict.get("name"))
            .map(VmValue::display)
            .filter(|name| !name.is_empty())
        {
            names.insert(name);
        }
    }
    names
}

/// Resolve the `tools` option into the flat list of tool dicts, accepting both
/// a bare list and a `{tools: [...]}` registry wrapper.
fn tool_entry_list(value: Option<&VmValue>) -> Option<Vec<VmValue>> {
    match value? {
        VmValue::List(items) => Some((**items).clone()),
        VmValue::Dict(dict) => match dict.get("tools") {
            Some(VmValue::List(items)) => Some((**items).clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Append a capability-gated guidance fragment for every active tool that
/// declares a `guidance` (or `system_guidance`) string. The fragment is gated
/// on the tool's own presence so instruction and tool can never drift.
fn append_tool_guidance_fragments(
    fragments: &mut Vec<crate::llm::prompt::PromptFragment>,
    options: Option<&BTreeMap<String, VmValue>>,
) {
    use crate::llm::prompt::{FragmentBucket, PromptFragment};
    let Some(list) = options.and_then(|options| tool_entry_list(options.get("tools"))) else {
        return;
    };
    for entry in list.iter() {
        let Some(dict) = entry.as_dict() else {
            continue;
        };
        let Some(name) = dict
            .get("name")
            .map(VmValue::display)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let guidance = dict
            .get("guidance")
            .or_else(|| dict.get("system_guidance"))
            .map(VmValue::display)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let Some(guidance) = guidance else {
            continue;
        };
        fragments.push(
            PromptFragment::new(
                format!("tool:{name}.guidance"),
                format!("tool:{name}"),
                FragmentBucket::Before,
                guidance,
            )
            .requiring_tools(vec![name]),
        );
    }
}

/// Expand the agent loop's `_system_fragments` channel — an ordered list of
/// `{id, source?, body, bucket?, requires_tools?}` dicts — into primary-region
/// [`PromptFragment`]s. This is how `agent_build_turn_system` ships the
/// primary block already decomposed into its parts, so the whole system prompt
/// (not just the host parts and reminders) is auditable through `assemble`.
///
/// Returns `true` if the channel was present (a list), in which case the
/// caller skips the single-string primary path. An empty list still counts as
/// present: the agent computed zero non-empty parts, so there is no primary.
fn append_decomposed_primary_fragments(
    fragments: &mut Vec<crate::llm::prompt::PromptFragment>,
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<bool, VmError> {
    use crate::llm::prompt::{FragmentBucket, PromptFragment};
    let Some(VmValue::List(items)) = options.and_then(|options| options.get("_system_fragments"))
    else {
        return Ok(false);
    };
    for (index, item) in items.iter().enumerate() {
        let Some(dict) = item.as_dict() else {
            continue;
        };
        let Some(body) = dict.get("body").map(VmValue::display) else {
            continue;
        };
        let id = dict
            .get("id")
            .map(VmValue::display)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| format!("primary[{index}]"));
        let source = dict
            .get("source")
            .map(VmValue::display)
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| "primary".to_string());
        let requires_tools = match dict.get("requires_tools") {
            Some(VmValue::List(tools)) => tools.iter().map(VmValue::display).collect(),
            _ => Vec::new(),
        };
        let bucket = match dict
            .get("bucket")
            .map(VmValue::display)
            .map(|bucket| bucket.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("before") => FragmentBucket::Before,
            Some("after") => FragmentBucket::After,
            Some(other) => {
                return Err(VmError::Runtime(format!(
                    "_system_fragments[{index}].bucket must be \"before\" or \"after\"; got {other:?}"
                )));
            }
        };
        fragments
            .push(PromptFragment::new(id, source, bucket, body).requiring_tools(requires_tools));
    }
    Ok(true)
}

fn assemble_ctx(options: Option<&BTreeMap<String, VmValue>>) -> crate::llm::prompt::AssembleCtx {
    crate::llm::prompt::AssembleCtx {
        tool_names: tool_names_from_options(options),
        caps: std::collections::BTreeSet::new(),
    }
}

/// Extract all LLM call options from the standard (prompt, system, options) args.
pub(crate) fn extract_llm_options(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    use crate::llm::api::{LlmApiMode, LlmCallOptions, ToolSearchMode, ToolSearchVariant};
    use crate::llm::provider::{provider_supports_defer_loading, provider_tool_search_variants};
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

    // If we're inside an `@step`-annotated persona function and the
    // call site didn't pin a model, inherit the step's declared model
    // and budget. The persona body stays free of "if step == X use
    // cheap model" branching.
    let mut options = options;
    apply_model_role_defaults(&mut options);
    apply_active_step_defaults(&mut options);

    let routing_policy = crate::llm::routing::extract_routing_policy(options.as_ref())?;
    let route_policy = parse_route_policy_option(options.as_ref())?;
    let mut provider = vm_resolve_provider(&options);
    let mut model = vm_resolve_model(&options, &provider);
    let routing_decision = resolve_route_policy(&route_policy, &provider, &model)?;
    if let Some(decision) = routing_decision.as_ref() {
        provider = decision.selected_provider.clone();
        model = decision.selected_model.clone();
    }
    // A routing_policy chain owns provider/model selection: snap the
    // base options to the first link so transcript-only consumers see
    // a sensible placeholder. The executor swaps these per attempt.
    if let Some(policy) = routing_policy.as_ref() {
        if let Some(first) = policy.chain.first() {
            provider = first.provider.clone();
            model = first.model.clone();
        }
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
    let caps = crate::llm::capabilities::lookup(&provider, &model);
    let api_mode = parse_api_mode_option(options.as_ref())?;
    if enforce_responses_provider_gate(api_mode, &provider) {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "api_mode: \"responses\" is only supported by provider \"openai\"; got provider \"{provider}\""
        )))));
    }
    let session_id = opt_str(&options, "session_id")
        .filter(|value| !value.is_empty())
        .or_else(crate::agent_sessions::current_session_id);
    let pending_reminders = pending_reminders_from_session(session_id.as_deref());
    let rendered_reminders = render_pending_reminders(&caps, &pending_reminders);
    let reminder_lifecycle = rendered_reminder_lifecycle(
        session_id.as_deref(),
        opt_int(&options, "_iteration").unwrap_or(0),
        &pending_reminders,
        &rendered_reminders,
    );
    let system =
        compose_system_prompt_with_reminders(system, options.as_ref(), &rendered_reminders)?;
    let enforce_capability_gates = !crate::llm::mock::cli_llm_mock_replay_active()
        && !crate::llm::mock::builtin_llm_mock_active();

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
    let logprobs = opt_bool(&options, "logprobs");
    let top_logprobs = opt_int(&options, "top_logprobs");
    let stop = opt_str_list(&options, "stop");
    let seed = opt_int(&options, "seed");
    let frequency_penalty =
        opt_float(&options, "frequency_penalty").or_else(|| default_float("frequency_penalty"));
    let presence_penalty =
        opt_float(&options, "presence_penalty").or_else(|| default_float("presence_penalty"));
    let timeout = resolve_timeout_secs(
        opt_int(&options, "timeout"),
        opt_int(&options, "timeout_ms"),
    );
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

    let reasoning_policy_application = crate::llm::reasoning_policy::resolve_for_llm_call(
        options.as_ref(),
        &provider,
        &model,
        &caps,
    )?;
    let thinking_from_reasoning_policy = reasoning_policy_application.is_some();
    let policy_thinking = reasoning_policy_application
        .as_ref()
        .map(|application| application.thinking.clone());

    let reasoning_effort = parse_reasoning_effort_option(options.as_ref())?;
    let thinking_from_reasoning_effort = reasoning_effort.is_some()
        && !options
            .as_ref()
            .and_then(|o| o.get("thinking"))
            .is_some_and(|value| value.is_truthy());
    let thinking = if let Some(level) = reasoning_effort {
        if options
            .as_ref()
            .and_then(|o| o.get("thinking"))
            .is_some_and(|value| value.is_truthy())
        {
            return Err(thinking_error(
                "reasoning_effort cannot be combined with a non-disabled thinking option",
            ));
        }
        crate::llm::api::ThinkingConfig::Effort { level }
    } else if let Some(thinking) = policy_thinking {
        thinking
    } else {
        parse_thinking_option(options.as_ref())?
    };
    let reasoning_effort_requires_provider_support = matches!(
        thinking,
        crate::llm::api::ThinkingConfig::Effort { level }
            if level != crate::llm::api::ReasoningEffort::None
    );
    if enforce_capability_gates
        && thinking_from_reasoning_effort
        && reasoning_effort_requires_provider_support
        && !caps.reasoning_effort_supported
    {
        return Err(unsupported_option_error(
            "reasoning_effort",
            &provider,
            &model,
        ));
    }
    if enforce_capability_gates {
        validate_thinking_supported(
            &thinking,
            &provider,
            &model,
            &caps.thinking_modes,
            if thinking_from_reasoning_effort {
                "reasoning_effort"
            } else if thinking_from_reasoning_policy {
                "reasoning_policy"
            } else {
                "thinking"
            },
        )?;
    }
    let mut anthropic_beta_features = parse_anthropic_beta_features_option(
        options.as_ref(),
        &thinking,
        &provider,
        &model,
        enforce_capability_gates,
    )?;

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
    if enforce_capability_gates {
        validate_output_format_supported(&output_format, &provider, &model, &caps)?;
    }
    let output_schema = output_schema.or_else(|| output_format.schema().cloned());
    // `schema_stream_abort` defaults to true whenever a schema is in play,
    // so callers that expect structured output get the early-abort win
    // automatically. Explicit `false` opts out and lets the stream run to
    // completion (relying on `schema_retries` for post-hoc recovery).
    let schema_stream_abort = match options.as_ref().and_then(|o| o.get("schema_stream_abort")) {
        Some(VmValue::Bool(value)) => *value,
        Some(VmValue::Nil) | None => output_schema.is_some(),
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                    "llm_call: `schema_stream_abort` must be a bool, got {}",
                    other.type_name()
                ),
            ))));
        }
    };

    // Reject the deprecated `transcript` option key. Conversation
    // lifecycle is expressed through `session_id` + the explicit
    // `agent_session_*` builtins; there is no opaque transcript dict to
    // pass around anymore.
    if options.as_ref().and_then(|o| o.get("transcript")).is_some() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
    let messages = apply_rendered_reminder_messages(messages, &rendered_reminders);
    let vision =
        opt_bool(&options, "vision") || crate::llm::content::messages_contain_images(&messages)?;
    let audio = option_is_enabled(options.as_ref(), "audio")
        || crate::llm::content::messages_contain_audio(&messages)?;
    let pdf = option_is_enabled(options.as_ref(), "pdf")
        || crate::llm::content::messages_contain_pdf(&messages)?;
    let uses_file_ids = crate::llm::content::messages_contain_file_ids(&messages)?;
    if enforce_capability_gates && vision && !caps.vision_supported {
        return Err(unsupported_option_error("vision", &provider, &model));
    }
    if enforce_capability_gates && audio && !caps.audio {
        return Err(unsupported_option_error("audio", &provider, &model));
    }
    if enforce_capability_gates && pdf && !caps.pdf {
        return Err(unsupported_option_error("pdf", &provider, &model));
    }
    if enforce_capability_gates && uses_file_ids && !caps.files_api_supported {
        return Err(unsupported_option_error("files_api", &provider, &model));
    }
    if uses_file_ids && caps.message_wire_format == "anthropic" {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut anthropic_beta_features,
            crate::stdlib::files::ANTHROPIC_FILES_API_BETA,
        );
    }
    if enforce_capability_gates && cache && !caps.prompt_caching {
        return Err(unsupported_option_error("cache", &provider, &model));
    }
    if vision
        && !crate::llm::provider::provider_supports_image_urls(&provider, &model)
        && crate::llm::content::messages_contain_url_images(&messages)?
    {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "llm_call: this provider/model route requires image base64; url image content is not supported",
        ))));
    }

    let tools_val = options
        .as_ref()
        .and_then(|o| o.get("tools"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    let tool_format = opt_str(&options, "tool_format")
        .unwrap_or_else(|| crate::llm_config::default_tool_format(&model, &provider));
    if enforce_capability_gates
        && tools_val.is_some()
        && tool_format == "native"
        && !caps.native_tools
    {
        return Err(unsupported_option_error("tools", &provider, &model));
    }
    let mut native_tools = if tool_format == "native" {
        if let Some(tools) = &tools_val {
            Some(vm_tools_to_native(tools, &provider, &model)?)
        } else {
            None
        }
    } else {
        None
    };
    let provider_tools = parse_provider_tools_option(options.as_ref())?;
    if enforce_capability_gates && !provider_tools.is_empty() && api_mode != LlmApiMode::Responses {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "provider_tools requires api_mode: \"responses\"",
        ))));
    }

    // tool_search option parsing: three shapes accepted.
    //   - shorthand string: "bm25" | "regex" | "hybrid" (mode: auto)
    //   - bool: true (defaults to bm25/auto), false (no tool_search)
    //   - dict: { variant, mode, strategy, always_loaded, name }
    // Unset / false / nil all leave tool_search absent — tools ship eagerly.
    let mut tool_search = parse_tool_search_option(options.as_ref())?;

    if let Some(cfg) = tool_search.as_mut() {
        // Resolve tool_search against the active provider now. Three
        // possible outcomes:
        //   - native: prepend the provider's meta-tool (Anthropic path
        //     for Claude 4.0+; OpenAI Responses-API path for GPT 5.4+).
        //   - client: leave the provider payload alone; the Harn stdlib
        //     agent loop filters deferred tools, injects the synthetic
        //     search tool, and emits client-mode events.
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
        if cfg.variant == ToolSearchVariant::Hybrid && cfg.mode == ToolSearchMode::Native {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "tool_search: variant \"hybrid\" is client-only; set mode: \"client\" or use \"bm25\"/\"regex\" for native provider tool search",
            ))));
        }
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
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
                if cfg.variant == ToolSearchVariant::Hybrid {
                    ToolSearchResolution::Client
                } else if provider_has_native {
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
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
                        // write `mode: "client"` for the stdlib agent
                        // loop fallback.
                        crate::llm::tools::apply_tool_search_native_injection_typed(
                            &mut native_tools,
                            shape,
                            cfg.variant.as_short(),
                            "hosted",
                        );
                    }
                }
            }
            ToolSearchResolution::Client => {}
        }
    }

    let tool_choice = options
        .as_ref()
        .and_then(|o| o.get("tool_choice"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(vm_value_to_json);
    // tool_choice is accepted for any route that can call tools at all —
    // native or text-format. Text-format routes don't have a protocol-level
    // tool_choice field, but the value is still meaningful (e.g. `"none"`
    // signals "skip tool calls this turn") and providers like Ollama
    // forward it through. Gating only on `native_tools` blocked scripts
    // that legitimately request tool_choice on text-tool routes such as
    // `ollama/qwen3.6:35b-a3b-coding-nvfp4`.
    if enforce_capability_gates
        && tool_choice.is_some()
        && !caps.native_tools
        && !caps.text_tool_wire_format_supported
    {
        return Err(unsupported_option_error("tool_choice", &provider, &model));
    }

    let provider_overrides = options
        .as_ref()
        .and_then(|o| o.get(&provider))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);
    let previous_response_id =
        opt_str(&options, "previous_response_id").filter(|value| !value.trim().is_empty());
    let store = opt_responses_store_field(options.as_ref())?;
    let background = opt_bool_field(options.as_ref(), "background")?;
    let truncation = opt_str(&options, "truncation").filter(|value| !value.trim().is_empty());
    let compact = opt_bool_field(options.as_ref(), "compact")?;
    let include = opt_str_list(&options, "include");
    let max_tool_calls = opt_int(&options, "max_tool_calls");

    if enforce_capability_gates
        && api_mode != LlmApiMode::Responses
        && (previous_response_id.is_some()
            || store.is_some()
            || background.is_some()
            || truncation.is_some()
            || compact.is_some()
            || include.is_some()
            || max_tool_calls.is_some())
    {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "Responses-only options require api_mode: \"responses\"",
        ))));
    }

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
    let reminders = options
        .as_ref()
        .and_then(|o| o.get("reminders"))
        .map(vm_value_to_json);

    // `fast: true` (or the provider-flavored `speed: "fast"`) opts into the
    // model's accelerated-serving tier. The catalog is the source of truth
    // for the per-provider knob, so we only validate the request is sane
    // here; the provider body builder reads `fast_mode.param`/`value`.
    let fast = opt_bool(&options, "fast") || opt_str(&options, "speed").as_deref() == Some("fast");
    if fast && enforce_capability_gates {
        match crate::llm::fast_mode::gate(&model) {
            crate::llm::fast_mode::FastModeGate::Usable => {}
            crate::llm::fast_mode::FastModeGate::Unsupported => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                    "fast: model \"{model}\" (provider \"{provider}\") has no accelerated-serving \
                     tier in the catalog; remove `fast` or pick a model that advertises `fast_mode`"
                ),
                ))));
            }
            crate::llm::fast_mode::FastModeGate::Deprecated { note } => {
                let detail = note.map(|n| format!(" ({n})")).unwrap_or_default();
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                    "fast: the accelerated-serving tier for model \"{model}\" is deprecated{detail}"
                ),
                ))));
            }
        }
    }

    let opts = LlmCallOptions {
        provider,
        model,
        api_key,
        api_mode,
        route_policy,
        fallback_chain,
        route_fallbacks,
        routing_decision,
        routing_policy,
        session_id,
        reminders,
        reminder_lifecycle,
        messages,
        system,
        transcript_summary: None,
        max_tokens,
        temperature,
        top_p,
        top_k,
        logprobs,
        top_logprobs,
        stop,
        seed,
        frequency_penalty,
        presence_penalty,
        fast,
        output_format,
        response_format,
        json_schema,
        output_schema,
        output_validation,
        schema_stream_abort,
        thinking,
        anthropic_beta_features,
        vision,
        tools: tools_val,
        native_tools,
        provider_tools,
        tool_choice,
        tool_search,
        cache,
        timeout,
        idle_timeout,
        stream,
        provider_overrides,
        previous_response_id,
        store,
        background,
        truncation,
        compact,
        include,
        max_tool_calls,
        budget,
        prefill,
        structural_experiment,
        applied_structural_experiment: None,
    };

    validate_options(&opts);
    Ok(opts)
}

fn thinking_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

fn parse_reasoning_effort_field(
    field: &str,
    raw: &str,
) -> Result<crate::llm::api::ReasoningEffort, VmError> {
    match raw {
        "none" => Ok(crate::llm::api::ReasoningEffort::None),
        "minimal" => Ok(crate::llm::api::ReasoningEffort::Minimal),
        "low" => Ok(crate::llm::api::ReasoningEffort::Low),
        "medium" => Ok(crate::llm::api::ReasoningEffort::Medium),
        "high" => Ok(crate::llm::api::ReasoningEffort::High),
        "xhigh" => Ok(crate::llm::api::ReasoningEffort::XHigh),
        other => Err(thinking_error(format!(
            "{field}: expected \"none\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\", got \"{other}\""
        ))),
    }
}

fn parse_reasoning_effort(raw: &str) -> Result<crate::llm::api::ReasoningEffort, VmError> {
    parse_reasoning_effort_field("thinking.level", raw)
}

fn parse_reasoning_effort_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Option<crate::llm::api::ReasoningEffort>, VmError> {
    let Some(raw) = options.and_then(|o| o.get("reasoning_effort")) else {
        return Ok(None);
    };
    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(None),
        VmValue::String(level) => parse_reasoning_effort_field("reasoning_effort", level).map(Some),
        other => Err(thinking_error(format!(
            "reasoning_effort: expected \"none\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\", got {}",
            other.type_name()
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
            "minimal" | "low" | "medium" | "high" | "xhigh" => Ok(ThinkingConfig::Effort {
                level: parse_reasoning_effort(s.as_ref())?,
            }),
            other => Err(thinking_error(format!(
                "thinking: expected bool, dict, or one of \"enabled\" | \"adaptive\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\", got \"{other}\""
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
    option_name: &str,
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
    Err(unsupported_option_error(option_name, provider, model))
}

fn parse_anthropic_beta_features_option(
    options: Option<&BTreeMap<String, VmValue>>,
    thinking: &crate::llm::api::ThinkingConfig,
    provider: &str,
    model: &str,
    enforce_capability_gates: bool,
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
                            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
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
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!(
                        "anthropic_beta_features: expected string or list<string>, got {}",
                        other.type_name()
                    ),
                ))));
            }
        }
    }

    let explicit_interleaved = options
        .and_then(|o| o.get("interleaved_thinking"))
        .is_some_and(|value| value.is_truthy());
    let caps = crate::llm::capabilities::lookup(provider, model);
    if enforce_capability_gates && explicit_interleaved && !caps.interleaved_thinking_supported {
        return Err(unsupported_option_error(
            "interleaved_thinking",
            provider,
            model,
        ));
    }
    if explicit_interleaved {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut features,
            crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
        );
    }

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
    Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
        "anthropic_beta_features: invalid beta feature name `{feature}`; expected ASCII letters, digits, '-' or '_'"
    )))))
}

/// Parse the `tool_search` option into a ToolSearchConfig.
///
/// Accepts:
///   - `nil` / absent / `false` → None (no tool_search engaged)
///   - `true` → default (bm25 + auto)
///   - `"bm25"` | `"regex"` | `"hybrid"` → that variant + auto
///   - `{ variant?, mode?, strategy?, always_loaded?, name? }` → explicit.
///     Non-string strategies are Harn-side custom scorers, so they force
///     client-mode resolution.
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
            "hybrid" => Ok(ToolSearchVariant::Hybrid),
            other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                    "tool_search.variant: expected \"bm25\", \"regex\", or \"hybrid\", got \"{other}\""
                ),
            )))),
        }
    };
    let mode_from_short = |s: &str| -> Result<ToolSearchMode, VmError> {
        match s {
            "auto" => Ok(ToolSearchMode::Auto),
            "native" => Ok(ToolSearchMode::Native),
            "client" => Ok(ToolSearchMode::Client),
            other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                "tool_search.mode: expected \"auto\" | \"native\" | \"client\", got \"{other}\""
            ),
            )))),
        }
    };
    let validate_strategy = |s: &str| -> Result<(), VmError> {
        match s {
            "bm25" | "regex" | "hybrid" => Ok(()),
            other => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!(
                    "tool_search.strategy: expected \"bm25\" | \"regex\" | \"hybrid\", got \"{other}\""
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
        })),
        VmValue::Dict(d) => {
            let variant = match d.get("variant") {
                Some(VmValue::String(s)) => variant_from_short(s.as_ref())?,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        "tool_search.variant: expected a string",
                    ))));
                }
                None => ToolSearchVariant::Bm25,
            };
            let mode = match d.get("mode") {
                Some(VmValue::String(s)) => mode_from_short(s.as_ref())?,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        "tool_search.mode: expected a string",
                    ))));
                }
                None => ToolSearchMode::Auto,
            };
            match d.get("always_loaded") {
                Some(VmValue::List(_)) | None => {}
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        "tool_search.always_loaded: expected a list of tool names",
                    ))));
                }
            }
            let custom_strategy = match d.get("strategy") {
                Some(VmValue::String(s)) => {
                    validate_strategy(s.as_ref())?;
                    false
                }
                Some(VmValue::Closure(_)) => true,
                Some(VmValue::Dict(strategy)) => {
                    if matches!(strategy.get("handler"), Some(VmValue::Closure(_))) {
                        true
                    } else {
                        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                            "tool_search.strategy: expected \"bm25\" | \"regex\" | \"hybrid\", a scorer closure, or {handler: closure}",
                        ))));
                    }
                }
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        "tool_search.strategy: expected \"bm25\" | \"regex\" | \"hybrid\", a scorer closure, or {handler: closure}",
                    ))));
                }
                None => false,
            };
            if custom_strategy && matches!(mode, ToolSearchMode::Native) {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "tool_search.strategy: custom scorers are client-only; set mode: \"client\" or \"auto\"",
                ))));
            }
            match d.get("name") {
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
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        "tool_search.name: expected a string",
                    ))));
                }
            };
            Ok(Some(ToolSearchConfig {
                variant: if custom_strategy {
                    ToolSearchVariant::Hybrid
                } else {
                    variant
                },
                mode,
            }))
        }
        _ => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "tool_search: expected bool, string (\"bm25\"/\"regex\"/\"hybrid\"), or dict \
             ({variant, mode, strategy, always_loaded, name})",
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
    let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
    let warn = |param: &str| {
        crate::events::log_warn(
            "llm",
            &format!(
                "\"{param}\" is not supported by provider \"{}\" model \"{}\", ignoring",
                opts.provider, opts.model
            ),
        );
    };

    if opts.seed.is_some() && !caps.seed_supported {
        warn("seed");
    }
    if opts.top_k.is_some() && !caps.top_k_supported {
        warn("top_k");
    }
    if opts.frequency_penalty.is_some() && !caps.frequency_penalty_supported {
        warn("frequency_penalty");
    }
    if opts.presence_penalty.is_some() && !caps.presence_penalty_supported {
        warn("presence_penalty");
    }
    if opts.cache && !caps.prompt_caching {
        warn("cache");
    }
}

#[cfg(test)]
mod output_format_tests {
    use super::*;

    #[test]
    fn parses_explicit_json_schema_output_format() {
        let mut fmt = BTreeMap::new();
        fmt.insert(
            "kind".to_string(),
            VmValue::String(std::sync::Arc::from("json_schema")),
        );
        fmt.insert(
            "schema".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "type".to_string(),
                VmValue::String(std::sync::Arc::from("object")),
            )]))),
        );
        fmt.insert("strict".to_string(), VmValue::Bool(false));
        let options = BTreeMap::from([(
            "output_format".to_string(),
            VmValue::Dict(std::sync::Arc::new(fmt)),
        )]);

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
            &crate::llm::capabilities::lookup("custom-provider", "custom-model"),
        )
        .expect_err("unsupported structured output should fail");

        assert!(err
            .to_string()
            .contains("option `output_format` is not supported by `custom-model`"));
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
            &crate::llm::capabilities::lookup("custom-provider", "custom-model"),
        )
        .expect("supported structured output");
        crate::llm::capabilities::clear_user_overrides();
    }
}

#[cfg(test)]
mod reminder_render_tests {
    use super::*;

    fn reminder(role_hint: ReminderRoleHint, body: &str) -> SystemReminder {
        SystemReminder {
            id: "reminder-1".to_string(),
            tags: vec!["test".to_string()],
            dedupe_key: None,
            ttl_turns: None,
            preserve_on_compact: false,
            propagate: crate::llm::helpers::ReminderPropagate::Session,
            role_hint,
            source: crate::llm::helpers::ReminderSource::InPipeline,
            body: body.to_string(),
            fired_at_turn: 0,
            originating_agent_id: None,
        }
    }

    #[test]
    fn anthropic_user_block_renders_as_xml_user_content_block() {
        crate::llm::capabilities::clear_user_overrides();
        let caps = crate::llm::capabilities::lookup("mock", "claude-sonnet-4-7");
        let rendered = render_pending_reminders(
            &caps,
            &[reminder(
                ReminderRoleHint::EphemeralCache,
                "remember <this>",
            )],
        );

        let RenderedReminder::Message(message) = &rendered[0] else {
            panic!("anthropic user block should render as a message");
        };
        assert_eq!(message["role"], "user");
        assert!(message["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<system-reminder>"));
        assert!(message["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("remember &lt;this&gt;"));
        assert_eq!(
            message["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn openai_developer_capability_renders_separate_developer_message() {
        crate::llm::capabilities::clear_user_overrides();
        let caps = crate::llm::capabilities::lookup("mock", "o3");
        let rendered = render_pending_reminders(
            &caps,
            &[reminder(ReminderRoleHint::System, "keep policy in mind")],
        );

        let RenderedReminder::Message(message) = &rendered[0] else {
            panic!("OpenAI developer route should render as a message");
        };
        assert_eq!(message["role"], "developer");
        assert_eq!(
            message["content"].as_str(),
            Some("System reminder:\nkeep policy in mind")
        );
    }

    #[test]
    fn gemini_xml_capability_renders_system_text_with_xml_scaffolding() {
        crate::llm::capabilities::clear_user_overrides();
        let caps = crate::llm::capabilities::lookup("gemini", "gemini-2.5-flash");
        let rendered =
            render_pending_reminders(&caps, &[reminder(ReminderRoleHint::System, "use context")]);

        let RenderedReminder::SystemText(text) = &rendered[0] else {
            panic!("Gemini route should fold reminder into system text");
        };
        assert_eq!(text, "<system-reminder>\nuse context\n</system-reminder>");
    }

    #[test]
    fn local_fallback_renders_plain_system_text() {
        let caps = crate::llm::capabilities::Capabilities::default();
        let rendered =
            render_pending_reminders(&caps, &[reminder(ReminderRoleHint::System, "plain")]);

        assert_eq!(
            rendered,
            vec![RenderedReminder::SystemText(
                "System reminder:\nplain".to_string()
            )]
        );
    }

    #[test]
    fn compose_system_prompt_places_reminders_before_appendix() {
        let options = BTreeMap::from([
            (
                "system_prompt_parts".to_string(),
                VmValue::String(std::sync::Arc::from("parts")),
            ),
            (
                "system_appendix".to_string(),
                VmValue::String(std::sync::Arc::from("appendix")),
            ),
        ]);
        let prompt = compose_system_prompt_with_reminders(
            Some("base".to_string()),
            Some(&options),
            &[RenderedReminder::SystemText("reminder".to_string())],
        )
        .expect("system prompt")
        .expect("non-empty prompt");

        assert_eq!(prompt, "parts\n\nbase\n\nreminder\n\nappendix");
    }

    fn s(text: &str) -> VmValue {
        VmValue::String(std::sync::Arc::from(text))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        VmValue::Dict(std::sync::Arc::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ))
    }

    fn list(items: Vec<VmValue>) -> VmValue {
        VmValue::List(std::sync::Arc::new(items))
    }

    #[test]
    fn full_host_option_ordering_is_faithful() {
        let options = BTreeMap::from([
            ("system_preamble".to_string(), s("P")),
            ("system_prefix".to_string(), s("X")),
            ("system_context".to_string(), s("C")),
            ("system_prompt_parts".to_string(), s("parts")),
            ("system_appendix".to_string(), s("A")),
            ("system_suffix".to_string(), s("S")),
        ]);
        let prompt = compose_system_prompt_with_reminders(
            Some("base".to_string()),
            Some(&options),
            &[RenderedReminder::SystemText("R".to_string())],
        )
        .expect("system prompt")
        .expect("non-empty prompt");
        // Before bucket in declaration order (preamble, prefix, context,
        // parts, primary, reminder), then After bucket (appendix, suffix).
        assert_eq!(prompt, "P\n\nX\n\nC\n\nparts\n\nbase\n\nR\n\nA\n\nS");
    }

    #[test]
    fn dict_part_position_override_moves_to_after() {
        let options = BTreeMap::from([(
            "system_prompt_parts".to_string(),
            dict(&[("content", s("moved")), ("position", s("after"))]),
        )]);
        let prompt = compose_system_prompt(Some("base".to_string()), Some(&options))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "base\n\nmoved");
    }

    #[test]
    fn dict_part_with_title_renders_heading() {
        let options = BTreeMap::from([(
            "system_prompt_parts".to_string(),
            dict(&[("content", s("body")), ("title", s("Title"))]),
        )]);
        let prompt = compose_system_prompt(None, Some(&options))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "## Title\nbody");
    }

    #[test]
    fn list_parts_expand_in_declaration_order() {
        let options = BTreeMap::from([(
            "system_prompt_parts".to_string(),
            list(vec![s("one"), s("two")]),
        )]);
        let prompt = compose_system_prompt(None, Some(&options))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "one\n\ntwo");
    }

    #[test]
    fn nil_system_arg_falls_back_to_opts_system() {
        let options = BTreeMap::from([("system".to_string(), s("fromopts"))]);
        let prompt = compose_system_prompt(None, Some(&options))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "fromopts");
    }

    #[test]
    fn tool_guidance_is_injected_only_when_the_tool_is_present() {
        // Tool carrying `guidance` → instruction auto-included after primary.
        let with_guidance = BTreeMap::from([(
            "tools".to_string(),
            list(vec![dict(&[
                ("name", s("todo")),
                ("description", s("Track tasks")),
                (
                    "guidance",
                    s("Always update the TODO tracker when working from a plan."),
                ),
            ])]),
        )]);
        let prompt = compose_system_prompt(Some("base".to_string()), Some(&with_guidance))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(
            prompt,
            "base\n\nAlways update the TODO tracker when working from a plan."
        );

        // Same tool without `guidance`, or a different tool set → no fragment.
        let no_guidance = BTreeMap::from([(
            "tools".to_string(),
            list(vec![dict(&[
                ("name", s("read")),
                ("description", s("Read files")),
            ])]),
        )]);
        let prompt = compose_system_prompt(Some("base".to_string()), Some(&no_guidance))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "base");
    }

    #[test]
    fn assemble_records_provenance_for_every_fragment() {
        let options = BTreeMap::from([
            ("system_prompt_parts".to_string(), s("parts")),
            (
                "tools".to_string(),
                list(vec![dict(&[
                    ("name", s("todo")),
                    ("description", s("Track tasks")),
                    ("guidance", s("Update the tracker.")),
                ])]),
            ),
        ]);
        let assembled = assemble_system_prompt(Some("base".to_string()), Some(&options), &[])
            .expect("assembled");
        // host:system_prompt_parts, primary, tool:todo.guidance — all included.
        let ids: Vec<&str> = assembled.provenance.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"host:system_prompt_parts"));
        assert!(ids.contains(&"primary"));
        assert!(ids.contains(&"tool:todo.guidance"));
        let todo = assembled
            .provenance
            .iter()
            .find(|t| t.id == "tool:todo.guidance")
            .expect("todo guidance trace");
        assert!(todo.included);
        assert!(todo.reason.contains("tool(s) present: todo"));
    }

    fn fragment(id: &str, body: &str) -> VmValue {
        dict(&[("id", s(id)), ("source", s("primary")), ("body", s(body))])
    }

    #[test]
    fn system_fragments_expand_in_place_of_the_single_primary() {
        // The decomposed channel yields the same bytes as the equivalent
        // joined-string primary, while keeping each part individually traced.
        let decomposed = BTreeMap::from([
            ("system_prefix".to_string(), s("X")),
            (
                "_system_fragments".to_string(),
                list(vec![
                    fragment("primary:system", "base"),
                    fragment("primary:active_skills", "## Active skills"),
                    fragment("primary:loop_contract", "Keep going until done."),
                ]),
            ),
            ("system_appendix".to_string(), s("A")),
        ]);
        let joined = "base\n\n## Active skills\n\nKeep going until done.";
        let baseline = BTreeMap::from([
            ("system_prefix".to_string(), s("X")),
            ("system_appendix".to_string(), s("A")),
        ]);

        let from_fragments = compose_system_prompt(None, Some(&decomposed))
            .expect("system prompt")
            .expect("non-empty prompt");
        let from_string = compose_system_prompt(Some(joined.to_string()), Some(&baseline))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(from_fragments, from_string);
        assert_eq!(
            from_fragments,
            "X\n\nbase\n\n## Active skills\n\nKeep going until done.\n\nA"
        );

        // Each part is its own provenance entry; there is no opaque `primary`.
        let assembled = assemble_system_prompt(None, Some(&decomposed), &[]).expect("assembled");
        let ids: Vec<&str> = assembled.provenance.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"primary:system"));
        assert!(ids.contains(&"primary:active_skills"));
        assert!(ids.contains(&"primary:loop_contract"));
        assert!(!ids.contains(&"primary"));
    }

    #[test]
    fn system_fragments_supersede_the_system_arg() {
        // When the channel is present, the `system` arg / `opts.system` no
        // longer contributes a primary fragment — the channel owns that block.
        let options = BTreeMap::from([
            ("system".to_string(), s("ignored opts.system")),
            (
                "_system_fragments".to_string(),
                list(vec![fragment("primary:system", "decomposed")]),
            ),
        ]);
        let prompt = compose_system_prompt(Some("ignored system arg".to_string()), Some(&options))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "decomposed");
    }

    #[test]
    fn empty_system_fragments_yield_no_primary() {
        // An empty list still claims the primary block: the agent computed zero
        // non-empty parts, so the `system` arg must not leak back in.
        let options = BTreeMap::from([("_system_fragments".to_string(), list(vec![]))]);
        let prompt = compose_system_prompt(Some("should not appear".to_string()), Some(&options))
            .expect("system prompt");
        assert_eq!(prompt, None);
    }

    #[test]
    fn system_fragments_honor_per_part_tool_gating() {
        let options = BTreeMap::from([(
            "_system_fragments".to_string(),
            list(vec![dict(&[
                ("id", s("primary:todo_nudge")),
                ("body", s("Keep the TODO list current.")),
                ("requires_tools", list(vec![s("todo")])),
            ])]),
        )]);
        // Tool absent → gated out, recorded with a reason.
        let assembled = assemble_system_prompt(None, Some(&options), &[]).expect("assembled");
        assert_eq!(assembled.system, None);
        let trace = assembled
            .provenance
            .iter()
            .find(|t| t.id == "primary:todo_nudge")
            .expect("nudge trace");
        assert!(!trace.included);
        assert!(trace.reason.contains("requires tool `todo`"));
    }

    #[test]
    fn system_fragments_can_target_the_tail_bucket() {
        let options = BTreeMap::from([
            (
                "_system_fragments".to_string(),
                list(vec![
                    fragment("primary:system", "base"),
                    dict(&[
                        ("id", s("primary:scratchpad")),
                        ("source", s("primary")),
                        ("body", s("scratchpad tail")),
                        ("bucket", s("after")),
                    ]),
                ]),
            ),
            ("system_suffix".to_string(), s("host suffix")),
        ]);

        let prompt = compose_system_prompt(None, Some(&options))
            .expect("system prompt")
            .expect("non-empty prompt");
        assert_eq!(prompt, "base\n\nhost suffix\n\nscratchpad tail");

        let assembled = assemble_system_prompt(None, Some(&options), &[]).expect("assembled");
        let trace = assembled
            .provenance
            .iter()
            .find(|t| t.id == "primary:scratchpad")
            .expect("scratchpad trace");
        assert_eq!(trace.bucket, "after");
    }

    #[test]
    fn system_fragments_reject_unknown_bucket() {
        let options = BTreeMap::from([(
            "_system_fragments".to_string(),
            list(vec![dict(&[
                ("id", s("primary:bad")),
                ("body", s("bad")),
                ("bucket", s("middle")),
            ])]),
        )]);
        let error = assemble_system_prompt(None, Some(&options), &[]).unwrap_err();
        assert!(
            error.to_string().contains("bucket must be"),
            "unexpected error: {error}"
        );
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::llm_config::{AliasDef, AuthEnv, ProviderDef, ProvidersConfig, TierRule};

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
            VmValue::String(std::sync::Arc::from(policy.to_string())),
        );
        options.insert(
            "fallback_chain".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                std::sync::Arc::from("fast".to_string()),
            )])),
        );
        extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
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

    fn extract_with_options(
        opts: BTreeMap<String, VmValue>,
    ) -> Result<crate::llm::api::LlmCallOptions, VmError> {
        extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(opts)),
        ])
    }

    fn model_role_options(role: &str) -> BTreeMap<String, VmValue> {
        BTreeMap::from([(
            "model_role".to_string(),
            VmValue::String(std::sync::Arc::from(role.to_string())),
        )])
    }

    fn clear_merge_role_env() {
        std::env::remove_var("HARN_LLM_MERGE_PROVIDER");
        std::env::remove_var("HARN_LLM_MERGE_MODEL");
        std::env::remove_var("HARN_LLM_MERGE_ROUTE_POLICY");
        std::env::remove_var("HARN_LLM_ROLE_MERGE_PROVIDER");
        std::env::remove_var("HARN_LLM_ROLE_MERGE_MODEL");
        std::env::remove_var("HARN_LLM_ROLE_MERGE_ROUTE_POLICY");
        std::env::remove_var("HARN_LLM_FAST_APPLY_PROVIDER");
        std::env::remove_var("HARN_LLM_FAST_APPLY_MODEL");
        std::env::remove_var("HARN_LLM_FAST_APPLY_ROUTE_POLICY");
        std::env::remove_var("HARN_LLM_ROLE_FAST_APPLY_PROVIDER");
        std::env::remove_var("HARN_LLM_ROLE_FAST_APPLY_MODEL");
        std::env::remove_var("HARN_LLM_ROLE_FAST_APPLY_ROUTE_POLICY");
    }

    #[test]
    fn model_role_defaults_fill_missing_llm_options() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        clear_merge_role_env();
        let mut overlay = ProvidersConfig::default();
        overlay.model_roles.insert(
            "merge".to_string(),
            BTreeMap::from([
                (
                    "provider".to_string(),
                    toml::Value::String("mock".to_string()),
                ),
                (
                    "model".to_string(),
                    toml::Value::String("mock-merge".to_string()),
                ),
                ("max_tokens".to_string(), toml::Value::Integer(4096)),
                ("temperature".to_string(), toml::Value::Float(0.0)),
            ]),
        );
        crate::llm_config::set_user_overrides(Some(overlay));
        super::super::reset_provider_key_cache();

        let opts = extract_with_options(model_role_options("merge")).expect("options");

        assert_eq!(opts.provider, "mock");
        assert_eq!(opts.model, "mock-merge");
        assert_eq!(opts.max_tokens, 4096);
        assert_eq!(opts.temperature, Some(0.0));

        crate::llm_config::clear_user_overrides();
        clear_merge_role_env();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn explicit_options_win_over_model_role_defaults() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        clear_merge_role_env();
        let mut overlay = ProvidersConfig::default();
        overlay.model_roles.insert(
            "merge".to_string(),
            BTreeMap::from([
                (
                    "provider".to_string(),
                    toml::Value::String("mock".to_string()),
                ),
                (
                    "model".to_string(),
                    toml::Value::String("mock-merge".to_string()),
                ),
                ("max_tokens".to_string(), toml::Value::Integer(4096)),
            ]),
        );
        crate::llm_config::set_user_overrides(Some(overlay));
        super::super::reset_provider_key_cache();

        let mut options = model_role_options("merge");
        options.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from("mock-explicit".to_string())),
        );
        options.insert("max_tokens".to_string(), VmValue::Int(512));
        let opts = extract_with_options(options).expect("options");

        assert_eq!(opts.provider, "mock");
        assert_eq!(opts.model, "mock-explicit");
        assert_eq!(opts.max_tokens, 512);

        crate::llm_config::clear_user_overrides();
        clear_merge_role_env();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn merge_model_role_has_env_overrides() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        crate::llm_config::clear_user_overrides();
        clear_merge_role_env();
        std::env::set_var("HARN_LLM_MERGE_PROVIDER", "mock");
        std::env::set_var("HARN_LLM_MERGE_MODEL", "mock-env-merge");
        super::super::reset_provider_key_cache();

        let opts = extract_with_options(model_role_options("merge")).expect("options");

        assert_eq!(opts.provider, "mock");
        assert_eq!(opts.model, "mock-env-merge");

        clear_merge_role_env();
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn model_role_aliases_do_not_override_exact_role_defaults() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        clear_merge_role_env();
        let mut overlay = ProvidersConfig::default();
        overlay.model_roles.insert(
            "fast_apply".to_string(),
            BTreeMap::from([
                (
                    "provider".to_string(),
                    toml::Value::String("mock".to_string()),
                ),
                (
                    "model".to_string(),
                    toml::Value::String("mock-fast-apply".to_string()),
                ),
            ]),
        );
        overlay.model_roles.insert(
            "merge".to_string(),
            BTreeMap::from([
                (
                    "provider".to_string(),
                    toml::Value::String("mock".to_string()),
                ),
                (
                    "model".to_string(),
                    toml::Value::String("mock-merge".to_string()),
                ),
            ]),
        );
        crate::llm_config::set_user_overrides(Some(overlay));
        super::super::reset_provider_key_cache();

        let merge_opts = extract_with_options(model_role_options("merge")).expect("merge options");
        let fast_apply_opts =
            extract_with_options(model_role_options("fast_apply")).expect("fast_apply options");

        assert_eq!(merge_opts.model, "mock-merge");
        assert_eq!(fast_apply_opts.model, "mock-fast-apply");

        crate::llm_config::clear_user_overrides();
        clear_merge_role_env();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn model_role_env_aliases_do_not_override_exact_role_env() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        crate::llm_config::clear_user_overrides();
        clear_merge_role_env();
        std::env::set_var("HARN_LLM_FAST_APPLY_PROVIDER", "mock");
        std::env::set_var("HARN_LLM_FAST_APPLY_MODEL", "mock-env-fast-apply");
        std::env::set_var("HARN_LLM_MERGE_PROVIDER", "mock");
        std::env::set_var("HARN_LLM_MERGE_MODEL", "mock-env-merge");
        super::super::reset_provider_key_cache();

        let merge_opts = extract_with_options(model_role_options("merge")).expect("merge options");
        let fast_apply_opts =
            extract_with_options(model_role_options("fast_apply")).expect("fast_apply options");

        assert_eq!(merge_opts.model, "mock-env-merge");
        assert_eq!(fast_apply_opts.model, "mock-env-fast-apply");

        clear_merge_role_env();
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();
    }

    #[test]
    fn model_role_config_keys_normalize_like_call_options() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        clear_merge_role_env();
        let mut overlay = ProvidersConfig::default();
        overlay.model_roles.insert(
            "fast-apply".to_string(),
            BTreeMap::from([
                (
                    "provider".to_string(),
                    toml::Value::String("mock".to_string()),
                ),
                (
                    "model".to_string(),
                    toml::Value::String("mock-fast-apply".to_string()),
                ),
            ]),
        );
        crate::llm_config::set_user_overrides(Some(overlay));
        super::super::reset_provider_key_cache();

        let opts = extract_with_options(model_role_options("fast_apply")).expect("options");

        assert_eq!(opts.provider, "mock");
        assert_eq!(opts.model, "mock-fast-apply");

        crate::llm_config::clear_user_overrides();
        clear_merge_role_env();
        super::super::reset_provider_key_cache();
    }

    fn fast_options(model: &str) -> BTreeMap<String, VmValue> {
        let mut options = BTreeMap::new();
        options.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from(model.to_string())),
        );
        options.insert("fast".to_string(), VmValue::Bool(true));
        options
    }

    #[test]
    fn fast_opts_into_tier_for_supported_model_and_guards_others() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        crate::llm_config::clear_user_overrides();
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        std::env::set_var("OPENAI_API_KEY", "test-key");
        super::super::reset_provider_key_cache();

        match extract_with_options(fast_options("claude-opus-4-8")) {
            Ok(opus) => assert!(opus.fast, "fast must be set for a model with a usable tier"),
            Err(e) => panic!("opus fast should succeed: {e:?}"),
        }

        // No fast tier -> rejected with a clear diagnostic.
        match extract_with_options(fast_options("gpt-4o")) {
            Err(VmError::Thrown(VmValue::String(message))) => {
                assert!(message.contains("no accelerated-serving tier"), "{message}");
            }
            other => panic!("expected thrown error for gpt-4o, got {:?}", other.is_ok()),
        }

        // Deprecated tier -> rejected.
        match extract_with_options(fast_options("claude-opus-4-6")) {
            Err(VmError::Thrown(VmValue::String(message))) => {
                assert!(message.contains("deprecated"), "{message}");
            }
            other => panic!(
                "expected thrown error for opus 4.6, got {:?}",
                other.is_ok()
            ),
        }

        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
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
            VmValue::String(std::sync::Arc::from("preference_list".to_string())),
        );
        policy.insert(
            "strategy".to_string(),
            VmValue::String(std::sync::Arc::from("cheapest_first".to_string())),
        );
        policy.insert(
            "prefer".to_string(),
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::String(std::sync::Arc::from("fast-mid")),
                VmValue::String(std::sync::Arc::from("cheap-mid")),
            ])),
        );
        let mut options = BTreeMap::new();
        options.insert(
            "route_policy".to_string(),
            VmValue::Dict(std::sync::Arc::new(policy)),
        );
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
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
            VmValue::String(std::sync::Arc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from("gpt-5.4".to_string())),
        );
        options.insert(
            "thinking".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "enabled".to_string(),
                VmValue::Bool(false),
            )]))),
        );
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("options");
        assert!(opts.thinking.is_disabled());
    }

    #[test]
    fn thinking_dict_enabled_budget_parses_typed_config() {
        let mut options = BTreeMap::new();
        options.insert(
            "provider".to_string(),
            VmValue::String(std::sync::Arc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from("claude-opus-4-6".to_string())),
        );
        options.insert(
            "thinking".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                (
                    "mode".to_string(),
                    VmValue::String(std::sync::Arc::from("enabled".to_string())),
                ),
                ("budget_tokens".to_string(), VmValue::Int(8000)),
            ]))),
        );
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
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
            VmValue::String(std::sync::Arc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from("claude-opus-4-6".to_string())),
        );
        options.insert(
            "anthropic_beta_features".to_string(),
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::String(std::sync::Arc::from(
                    "fine-grained-tool-streaming-2025-05-14",
                )),
                VmValue::String(std::sync::Arc::from(
                    crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
                )),
            ])),
        );
        options.insert("interleaved_thinking".to_string(), VmValue::Bool(true));

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
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
                VmValue::String(std::sync::Arc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("claude-opus-4-6".to_string())),
            ),
            (
                "anthropic_beta_features".to_string(),
                VmValue::String(std::sync::Arc::from("bad\r\nheader".to_string())),
            ),
        ]);

        let err = match extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
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
            VmValue::String(std::sync::Arc::from("mock".to_string())),
        );
        options.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from("o3".to_string())),
        );
        options.insert(
            "thinking".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                (
                    "mode".to_string(),
                    VmValue::String(std::sync::Arc::from("effort".to_string())),
                ),
                (
                    "level".to_string(),
                    VmValue::String(std::sync::Arc::from("high".to_string())),
                ),
            ]))),
        );
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("options");
        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::High
            }
        );
    }

    fn unsupported_local_options(extra: Vec<(&str, VmValue)>) -> VmError {
        let mut options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("local".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("unsupported-model".to_string())),
            ),
        ]);
        for (key, value) in extra {
            options.insert(key.to_string(), value);
        }
        match extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ]) {
            Ok(_) => panic!("unsupported option should fail"),
            Err(err) => err,
        }
    }

    fn assert_unsupported_local_option(option: &str, extra: Vec<(&str, VmValue)>) {
        crate::llm::capabilities::clear_user_overrides();
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();

        let err = unsupported_local_options(extra);

        assert!(
            err.to_string().contains(&format!(
                "option `{option}` is not supported by `unsupported-model` (provider `local`). See `harn providers matrix` for compatibility."
            )),
            "unexpected error for {option}: {err}"
        );
    }

    fn one_tool_list() -> VmValue {
        VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("lookup")),
                ),
                (
                    "description".to_string(),
                    VmValue::String(std::sync::Arc::from("Look something up")),
                ),
                (
                    "parameters".to_string(),
                    VmValue::Dict(std::sync::Arc::new(BTreeMap::new())),
                ),
            ])),
        )]))
    }

    #[test]
    fn unsupported_capability_options_error_with_provider_matrix_hint() {
        assert_unsupported_local_option("thinking", vec![("thinking", VmValue::Bool(true))]);
        assert_unsupported_local_option(
            "output_format",
            vec![(
                "output_format",
                VmValue::String(std::sync::Arc::from("json_object".to_string())),
            )],
        );
        assert_unsupported_local_option(
            "tools",
            vec![
                (
                    "tool_format",
                    VmValue::String(std::sync::Arc::from("native".to_string())),
                ),
                ("tools", one_tool_list()),
            ],
        );
        assert_unsupported_local_option("cache", vec![("cache", VmValue::Bool(true))]);
        assert_unsupported_local_option("vision", vec![("vision", VmValue::Bool(true))]);
        assert_unsupported_local_option("audio", vec![("audio", VmValue::Bool(true))]);
        assert_unsupported_local_option("pdf", vec![("pdf", VmValue::Bool(true))]);
        assert_unsupported_local_option(
            "reasoning_effort",
            vec![(
                "reasoning_effort",
                VmValue::String(std::sync::Arc::from("high".to_string())),
            )],
        );
        assert_unsupported_local_option(
            "interleaved_thinking",
            vec![("interleaved_thinking", VmValue::Bool(true))],
        );
    }

    #[test]
    fn tool_choice_accepted_on_text_tool_routes() {
        // qwen3.6 on Ollama is native_tools=false but
        // text_tool_wire_format_supported=true. tool_choice should not
        // be rejected on text-format routes (e.g. agent scripts that
        // pass tool_choice="none" to suppress further tool calls).
        crate::llm::capabilities::clear_user_overrides();
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();

        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("ollama".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from(
                    "qwen3.6:35b-a3b-coding-nvfp4".to_string(),
                )),
            ),
            (
                "tool_choice".to_string(),
                VmValue::String(std::sync::Arc::from("none".to_string())),
            ),
        ]);
        extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("tool_choice accepted on text-format routes");
    }

    #[test]
    fn text_tool_format_does_not_emit_native_provider_tools() {
        crate::llm::capabilities::clear_user_overrides();
        crate::llm_config::clear_user_overrides();
        super::super::reset_provider_key_cache();

        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("ollama".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("devstral-small-2:24b".to_string())),
            ),
            (
                "tool_format".to_string(),
                VmValue::String(std::sync::Arc::from("text".to_string())),
            ),
            ("tools".to_string(), one_tool_list()),
        ]);
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("text-format tools accepted");

        assert!(opts.tools.is_some());
        assert!(opts.native_tools.is_none());
    }

    #[test]
    fn standalone_reasoning_effort_maps_to_thinking_effort_when_supported() {
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("o3".to_string())),
            ),
            (
                "reasoning_effort".to_string(),
                VmValue::String(std::sync::Arc::from("high".to_string())),
            ),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("supported reasoning_effort");

        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::High
            }
        );
    }

    #[test]
    fn standalone_reasoning_effort_accepts_minimal_when_supported() {
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("o3".to_string())),
            ),
            (
                "reasoning_effort".to_string(),
                VmValue::String(std::sync::Arc::from("minimal".to_string())),
            ),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("minimal reasoning_effort");

        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::Minimal
            }
        );
    }

    #[test]
    fn reasoning_policy_maps_to_provider_aware_thinking_when_explicit() {
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("gpt-5.5".to_string())),
            ),
            (
                "reasoning_policy".to_string(),
                VmValue::String(std::sync::Arc::from("off".to_string())),
            ),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("reasoning_policy");

        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::None
            }
        );
    }

    #[test]
    fn session_pinned_reasoning_policy_is_llm_call_default() {
        crate::agent_sessions::reset_session_store();
        let session_id = crate::agent_sessions::open_or_create(Some(
            "reasoning-policy-options-session".to_string(),
        ));
        crate::agent_sessions::set_pinned_reasoning_policy(&session_id, Some("high".to_string()))
            .expect("set policy");
        let _session_guard = crate::agent_sessions::enter_current_session(session_id);
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("o3".to_string())),
            ),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("session reasoning_policy");

        drop(_session_guard);
        crate::agent_sessions::reset_session_store();

        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::High
            }
        );
    }

    #[test]
    fn explicit_thinking_wins_over_session_pinned_reasoning_policy() {
        crate::agent_sessions::reset_session_store();
        let session_id = crate::agent_sessions::open_or_create(Some(
            "reasoning-policy-explicit-session".to_string(),
        ));
        crate::agent_sessions::set_pinned_reasoning_policy(&session_id, Some("high".to_string()))
            .expect("set policy");
        let _session_guard = crate::agent_sessions::enter_current_session(session_id);
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("o3".to_string())),
            ),
            ("thinking".to_string(), VmValue::Bool(false)),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("explicit thinking");

        drop(_session_guard);
        crate::agent_sessions::reset_session_store();

        assert!(opts.thinking.is_disabled());
    }

    #[test]
    fn standalone_reasoning_effort_accepts_none_and_xhigh_when_supported() {
        for (raw, expected) in [
            ("none", crate::llm::api::ReasoningEffort::None),
            ("xhigh", crate::llm::api::ReasoningEffort::XHigh),
        ] {
            let options = BTreeMap::from([
                (
                    "provider".to_string(),
                    VmValue::String(std::sync::Arc::from("mock".to_string())),
                ),
                (
                    "model".to_string(),
                    VmValue::String(std::sync::Arc::from("gpt-5.5".to_string())),
                ),
                (
                    "reasoning_effort".to_string(),
                    VmValue::String(std::sync::Arc::from(raw.to_string())),
                ),
            ]);

            let opts = extract_llm_options(&[
                VmValue::String(std::sync::Arc::from("hello".to_string())),
                VmValue::Nil,
                VmValue::Dict(std::sync::Arc::new(options)),
            ])
            .expect("reasoning_effort");

            assert_eq!(
                opts.thinking,
                crate::llm::api::ThinkingConfig::Effort { level: expected }
            );
        }
    }

    #[test]
    fn standalone_reasoning_effort_none_disables_thinking_without_effort_capability() {
        let options = BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("local".to_string())),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("no-effort-model".to_string())),
            ),
            (
                "reasoning_effort".to_string(),
                VmValue::String(std::sync::Arc::from("none".to_string())),
            ),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(options)),
        ])
        .expect("reasoning_effort none should be universally accepted");

        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::None
            }
        );
    }

    #[test]
    fn standalone_reasoning_effort_uses_dedicated_capability_gate() {
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.local]]
model_match = "thinking-effort-only"
thinking_modes = ["effort"]
"#,
        )
        .expect("capability override");
        super::super::reset_provider_key_cache();

        let err = unsupported_local_options(vec![
            (
                "model",
                VmValue::String(std::sync::Arc::from("thinking-effort-only".to_string())),
            ),
            (
                "reasoning_effort",
                VmValue::String(std::sync::Arc::from("high".to_string())),
            ),
        ]);

        assert!(
            err.to_string()
                .contains("option `reasoning_effort` is not supported"),
            "unexpected error: {err}"
        );
        crate::llm::capabilities::clear_user_overrides();
    }

    #[test]
    fn image_content_sets_vision_and_requires_capability() {
        let image_block = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "type".to_string(),
                VmValue::String(std::sync::Arc::from("image")),
            ),
            (
                "base64".to_string(),
                VmValue::String(std::sync::Arc::from("iVBORw0KGgo=")),
            ),
            (
                "media_type".to_string(),
                VmValue::String(std::sync::Arc::from("image/png")),
            ),
        ])));
        let message = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "role".to_string(),
                VmValue::String(std::sync::Arc::from("user")),
            ),
            (
                "content".to_string(),
                VmValue::List(std::sync::Arc::new(vec![image_block])),
            ),
        ])));
        let options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock")),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("gpt-4o")),
            ),
            (
                "messages".to_string(),
                VmValue::List(std::sync::Arc::new(vec![message.clone()])),
            ),
        ])));
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("")),
            VmValue::Nil,
            options,
        ])
        .unwrap();
        assert!(opts.vision);

        let bad_options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock")),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("gpt-3.5-turbo")),
            ),
            (
                "messages".to_string(),
                VmValue::List(std::sync::Arc::new(vec![message])),
            ),
        ])));
        let err = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("")),
            VmValue::Nil,
            bad_options,
        ])
        .err()
        .expect("non-vision model should reject image content");
        assert!(err.to_string().contains("option `vision` is not supported"));

        let url_image = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "type".to_string(),
                VmValue::String(std::sync::Arc::from("image")),
            ),
            (
                "url".to_string(),
                VmValue::String(std::sync::Arc::from("https://example.com/image.png")),
            ),
            (
                "media_type".to_string(),
                VmValue::String(std::sync::Arc::from("image/png")),
            ),
        ])));
        let url_message = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "role".to_string(),
                VmValue::String(std::sync::Arc::from("user")),
            ),
            (
                "content".to_string(),
                VmValue::List(std::sync::Arc::new(vec![url_image])),
            ),
        ])));
        let ollama_options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("ollama")),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("llava:latest")),
            ),
            (
                "messages".to_string(),
                VmValue::List(std::sync::Arc::new(vec![url_message])),
            ),
        ])));
        let err = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("")),
            VmValue::Nil,
            ollama_options,
        ])
        .err()
        .expect("ollama should reject url image content");
        assert!(err.to_string().contains("requires image base64"));
    }

    #[test]
    fn pdf_and_audio_content_require_capabilities() {
        let pdf_block = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "type".to_string(),
                VmValue::String(std::sync::Arc::from("pdf")),
            ),
            (
                "file_id".to_string(),
                VmValue::String(std::sync::Arc::from("file_123")),
            ),
        ])));
        let audio_block = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "type".to_string(),
                VmValue::String(std::sync::Arc::from("audio")),
            ),
            (
                "base64".to_string(),
                VmValue::String(std::sync::Arc::from("UklGRg==")),
            ),
            (
                "media_type".to_string(),
                VmValue::String(std::sync::Arc::from("audio/wav")),
            ),
        ])));
        let message = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "role".to_string(),
                VmValue::String(std::sync::Arc::from("user")),
            ),
            (
                "content".to_string(),
                VmValue::List(std::sync::Arc::new(vec![pdf_block, audio_block])),
            ),
        ])));
        let options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock")),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("claude-sonnet-4-7")),
            ),
            (
                "messages".to_string(),
                VmValue::List(std::sync::Arc::new(vec![message.clone()])),
            ),
        ])));
        let opts = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("")),
            VmValue::Nil,
            options,
        ])
        .unwrap();
        assert!(opts
            .anthropic_beta_features
            .contains(&crate::stdlib::files::ANTHROPIC_FILES_API_BETA.to_string()));

        let bad_options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("mock")),
            ),
            (
                "model".to_string(),
                VmValue::String(std::sync::Arc::from("gpt-3.5-turbo")),
            ),
            (
                "messages".to_string(),
                VmValue::List(std::sync::Arc::new(vec![message])),
            ),
        ])));
        let err = extract_llm_options(&[
            VmValue::String(std::sync::Arc::from("")),
            VmValue::Nil,
            bad_options,
        ])
        .err()
        .expect("non-multimodal model should reject pdf/audio content");
        assert!(err.to_string().contains("option `audio` is not supported"));
    }
}
