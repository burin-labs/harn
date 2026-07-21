//! Catalog query surface: provider/model lookups, per-role and per-model
//! parameter defaults, pricing, capability tags, tool-format resolution, and
//! tier candidate enumeration.
use std::collections::BTreeMap;

use super::*;

use harn_glob::match_name as glob_match;

const LOGICAL_MODEL_DEFAULT_PREFIX: &str = "logical:";
const MODEL_DEFAULT_UNSET_KEY: &str = "_unset";

/// Get provider config for resolving base_url, auth, etc.
pub fn provider_config(name: &str) -> Option<ProviderDef> {
    let mut provider = effective_config().providers.get(name).cloned()?;
    if let Some(base_url) = runtime_provider_endpoint(name) {
        // The endpoint was host-verified for this execution. Clear catalog
        // selectors only on this clone so every transport path resolves the
        // same endpoint without making runtime state serializable or public.
        provider.base_url = base_url;
        provider.base_url_env = None;
        provider.region_env = None;
    }
    Some(provider)
}

pub fn provider_protocol(name: &str) -> Option<String> {
    provider_config(name).and_then(|def| def.protocol)
}

pub fn provider_uses_acp(name: &str) -> bool {
    provider_protocol(name)
        .as_deref()
        .is_some_and(|protocol| protocol.eq_ignore_ascii_case("acp"))
}

/// Get model-specific default parameters (temperature, etc.).
/// Matches glob patterns in model_defaults keys.
pub fn model_params(model_id: &str) -> BTreeMap<String, toml::Value> {
    let config = effective_config();
    matching_model_params(&config, model_id)
}

fn matching_model_params(
    config: &ProvidersConfig,
    model_id: &str,
) -> BTreeMap<String, toml::Value> {
    let mut params = BTreeMap::new();
    apply_matching_model_params(config, model_id, &mut params);
    params
}

fn apply_matching_model_params(
    config: &ProvidersConfig,
    model_id: &str,
    params: &mut BTreeMap<String, toml::Value>,
) {
    for (pattern, defaults) in &config.model_defaults {
        if !pattern.starts_with(LOGICAL_MODEL_DEFAULT_PREFIX) && glob_match(pattern, model_id) {
            apply_model_param_layer(params, defaults);
        }
    }
}

fn apply_model_param_layer(
    params: &mut BTreeMap<String, toml::Value>,
    defaults: &BTreeMap<String, toml::Value>,
) {
    for (key, value) in defaults {
        if key != MODEL_DEFAULT_UNSET_KEY {
            params.insert(key.clone(), value.clone());
        }
    }
    if let Some(keys) = defaults
        .get(MODEL_DEFAULT_UNSET_KEY)
        .and_then(toml::Value::as_array)
    {
        for key in keys.iter().filter_map(toml::Value::as_str) {
            params.remove(key);
        }
    }
}

/// Get generation defaults for one concrete serving route.
///
/// Publisher/model defaults use an exact `logical:<logical_model>` selector.
/// Route-id patterns then override them, preserving the existing precedence
/// where a provider-qualified pattern wins over a bare wire-model pattern.
pub fn model_params_for_route(provider: &str, model_id: &str) -> BTreeMap<String, toml::Value> {
    let config = effective_config();
    model_params_for_route_with_config(&config, provider, model_id)
}

/// Return the Harn-validated generation defaults that are safe to persist in
/// an execution receipt.
///
/// Route-specific overlays remain intentionally free-form so operators can
/// configure provider-specific request parameters. Those fields must still
/// influence inference, but they are not part of Harn's stable, secret-free
/// receipt contract. Keep this filter beside Harn's generation validator so hosts do
/// not duplicate the generation-default schema.
pub fn generation_defaults_for_route(
    provider: &str,
    model_id: &str,
) -> BTreeMap<String, toml::Value> {
    model_params_for_route(provider, model_id)
        .into_iter()
        .filter(|(key, value)| is_valid_generation_default(key, value))
        .collect()
}

pub(crate) fn model_params_for_route_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> BTreeMap<String, toml::Value> {
    let normalized_id = normalize_model_id(model_id);
    let route = config
        .models
        .get_key_value(model_id)
        .filter(|(_, model)| model.provider == provider)
        .or_else(|| {
            config
                .models
                .get_key_value(&normalized_id)
                .filter(|(_, model)| model.provider == provider)
        })
        .or_else(|| {
            config.models.iter().find(|(_, model)| {
                model.provider == provider
                    && model
                        .wire_model
                        .as_deref()
                        .is_some_and(|wire| wire == model_id || wire == normalized_id.as_str())
            })
        });

    let mut params = route
        .and_then(|(_, model)| model.logical_model.as_deref())
        .and_then(|logical_model| {
            config
                .model_defaults
                .get(&format!("{LOGICAL_MODEL_DEFAULT_PREFIX}{logical_model}"))
        })
        .map(|defaults| {
            let mut params = BTreeMap::new();
            apply_model_param_layer(&mut params, defaults);
            params
        })
        .unwrap_or_default();

    let mut identities = vec![model_id.to_string()];
    if normalized_id != model_id {
        identities.push(normalized_id);
    }
    if let Some((catalog_id, model)) = route {
        for identity in [Some(catalog_id.as_str()), model.wire_model.as_deref()]
            .into_iter()
            .flatten()
        {
            if !identities.iter().any(|known| known == identity) {
                identities.push(identity.to_string());
            }
        }
    }
    for identity in &identities {
        apply_matching_model_params(config, identity, &mut params);
    }
    let provider_prefix = format!("{provider}/");
    for identity in identities {
        if !identity.starts_with(&provider_prefix) {
            apply_matching_model_params(
                config,
                &format!("{provider_prefix}{identity}"),
                &mut params,
            );
        }
    }
    params
}

/// Validate logical-model defaults against catalog identities and route caps.
/// Route-specific patterns remain intentionally free-form for operator
/// overrides; `logical:` selectors are publisher-level contracts and must be
/// exact, typed, and representable by every route that inherits them.
pub fn model_default_issues(config: &ProvidersConfig) -> Vec<String> {
    let mut issues = Vec::new();
    for (selector, defaults) in &config.model_defaults {
        if let Some(unset) = defaults.get(MODEL_DEFAULT_UNSET_KEY) {
            let valid = !selector.starts_with(LOGICAL_MODEL_DEFAULT_PREFIX)
                && unset.as_array().is_some_and(|keys| {
                    !keys.is_empty()
                        && keys.iter().all(|key| {
                            key.as_str()
                                .is_some_and(is_supported_generation_default_key)
                        })
                });
            if !valid {
                issues.push(format!(
                    "model_defaults.{selector}.{MODEL_DEFAULT_UNSET_KEY} must be a non-empty list of supported route-default keys"
                ));
            }
        }
        let Some(logical_model) = selector.strip_prefix(LOGICAL_MODEL_DEFAULT_PREFIX) else {
            continue;
        };
        if logical_model.is_empty()
            || logical_model.contains('*')
            || logical_model.contains('?')
            || logical_model.contains('[')
        {
            issues.push(format!(
                "model_defaults.{selector} must name one exact logical model"
            ));
            continue;
        }
        let routes: Vec<_> = config
            .models
            .iter()
            .filter(|(_, model)| model.logical_model.as_deref() == Some(logical_model))
            .collect();
        if routes.is_empty() {
            issues.push(format!(
                "model_defaults.{selector} references an unknown logical model"
            ));
            continue;
        }

        for (key, value) in defaults {
            if key == MODEL_DEFAULT_UNSET_KEY {
                continue;
            }
            if !is_valid_generation_default(key, value) {
                issues.push(format!(
                    "model_defaults.{selector}.{key} is not a supported generation default"
                ));
                continue;
            }

            for (model_id, model) in &routes {
                let caps = crate::llm::capabilities::lookup_with_user_overrides(
                    &model.provider,
                    model_id,
                    None,
                );
                let supported = match key.as_str() {
                    "temperature" => caps.temperature_supported,
                    "top_p" => caps.top_p_supported,
                    "top_k" => caps.top_k_supported,
                    "frequency_penalty" => caps.frequency_penalty_supported,
                    "presence_penalty" => caps.presence_penalty_supported,
                    "reasoning_effort" => {
                        let effort = value.as_str().expect("validated effort string");
                        caps.reasoning_effort_supported
                            && caps.thinking_modes.iter().any(|mode| mode == "effort")
                            && (caps.reasoning_effort_levels.is_empty()
                                || caps
                                    .reasoning_effort_levels
                                    .iter()
                                    .any(|level| level == effort))
                    }
                    "max_tokens" => true,
                    _ => false,
                };
                let effective =
                    model_params_for_route_with_config(config, &model.provider, model_id);
                if !supported && effective.contains_key(key) {
                    issues.push(format!(
                        "model_defaults.{selector}.{key} cannot be represented by route {}:{}",
                        model.provider, model_id
                    ));
                }
            }
        }
    }
    issues
}

fn is_supported_generation_default_key(key: &str) -> bool {
    matches!(
        key,
        "temperature"
            | "top_p"
            | "top_k"
            | "frequency_penalty"
            | "presence_penalty"
            | "max_tokens"
            | "reasoning_effort"
    )
}

fn is_valid_generation_default(key: &str, value: &toml::Value) -> bool {
    match key {
        "temperature" => value
            .as_float()
            .is_some_and(|value| value.is_finite() && (0.0..=2.0).contains(&value)),
        "frequency_penalty" | "presence_penalty" => value
            .as_float()
            .is_some_and(|value| value.is_finite() && (-2.0..=2.0).contains(&value)),
        "top_p" => value
            .as_float()
            .is_some_and(|value| value.is_finite() && (0.0..=1.0).contains(&value)),
        "top_k" => value.as_integer().is_some_and(|value| value >= 0),
        "max_tokens" => value.as_integer().is_some_and(|value| value > 0),
        "reasoning_effort" => value.as_str().is_some_and(|value| {
            matches!(
                value,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            )
        }),
        _ => false,
    }
}

/// Get per-role LLM defaults, e.g. `[model_roles.merge]`.
///
/// Role defaults are intentionally shaped like ordinary `llm_call` options:
/// callers can pin `provider`/`model`, install `route_policy` or `prefer`,
/// and tune budget/latency knobs without creating a parallel routing stack.
/// Environment variables provide a lightweight operational override for
/// merge/fast-apply workers:
///
/// - `HARN_LLM_MERGE_PROVIDER`, `HARN_LLM_MERGE_MODEL`,
///   `HARN_LLM_MERGE_ROUTE_POLICY`
/// - `HARN_LLM_FAST_APPLY_PROVIDER`, `HARN_LLM_FAST_APPLY_MODEL`,
///   `HARN_LLM_FAST_APPLY_ROUTE_POLICY`
/// - `HARN_LLM_ROLE_<ROLE>_PROVIDER`, `_MODEL`, `_ROUTE_POLICY`
pub fn model_role_defaults(role: &str) -> BTreeMap<String, toml::Value> {
    let normalized = normalize_model_role_name(role);
    if normalized.is_empty() {
        return BTreeMap::new();
    }
    let config = effective_config();
    let mut params = BTreeMap::new();
    for key in role_lookup_keys(&normalized) {
        extend_model_role_defaults(&config, &key, &mut params);
    }
    apply_model_role_env_overrides(&normalized, &mut params);
    params
}

fn extend_model_role_defaults(
    config: &ProvidersConfig,
    role: &str,
    params: &mut BTreeMap<String, toml::Value>,
) {
    for (configured_role, defaults) in &config.model_roles {
        if normalize_model_role_name(configured_role) == role {
            params.extend(defaults.clone());
        }
    }
    if let Some(defaults) = config.model_roles.get(role) {
        params.extend(defaults.clone());
    }
}

fn normalize_model_role_name(role: &str) -> String {
    role.trim().to_ascii_lowercase().replace('-', "_")
}

fn role_lookup_keys(role: &str) -> Vec<String> {
    if role == "merge" {
        vec!["fast_apply".to_string(), "merge".to_string()]
    } else if role == "fast_apply" {
        vec!["merge".to_string(), "fast_apply".to_string()]
    } else {
        vec![role.to_string()]
    }
}

fn role_env_token(role: &str) -> String {
    role.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn apply_model_role_env_overrides(role: &str, params: &mut BTreeMap<String, toml::Value>) {
    for alias in role_env_aliases(role) {
        apply_model_role_env_var(&format!("HARN_LLM_{alias}_PROVIDER"), "provider", params);
        apply_model_role_env_var(&format!("HARN_LLM_{alias}_MODEL"), "model", params);
        apply_model_role_env_var(
            &format!("HARN_LLM_{alias}_ROUTE_POLICY"),
            "route_policy",
            params,
        );
        apply_model_role_env_var(
            &format!("HARN_LLM_ROLE_{alias}_PROVIDER"),
            "provider",
            params,
        );
        apply_model_role_env_var(&format!("HARN_LLM_ROLE_{alias}_MODEL"), "model", params);
        apply_model_role_env_var(
            &format!("HARN_LLM_ROLE_{alias}_ROUTE_POLICY"),
            "route_policy",
            params,
        );
    }
}

fn role_env_aliases(role: &str) -> Vec<String> {
    let token = role_env_token(role);
    if token.is_empty() {
        return Vec::new();
    }
    if token == "MERGE" {
        vec!["FAST_APPLY".to_string(), "MERGE".to_string()]
    } else if token == "FAST_APPLY" {
        vec!["MERGE".to_string(), "FAST_APPLY".to_string()]
    } else {
        vec![token]
    }
}

fn apply_model_role_env_var(
    env_name: &str,
    option_name: &str,
    params: &mut BTreeMap<String, toml::Value>,
) {
    let Ok(value) = std::env::var(env_name) else {
        return;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    params.insert(
        option_name.to_string(),
        toml::Value::String(trimmed.to_string()),
    );
}

/// Get list of configured provider names.
pub fn provider_names() -> Vec<String> {
    effective_config().providers.keys().cloned().collect()
}

/// Return every configured alias name, sorted deterministically.
pub fn known_model_names() -> Vec<String> {
    effective_config().aliases.keys().cloned().collect()
}

pub fn alias_entries() -> Vec<(String, AliasDef)> {
    effective_config()
        .aliases
        .iter()
        .map(|(name, alias)| (name.clone(), alias.clone()))
        .collect()
}

pub fn alias_tool_calling_entry(alias: &str) -> Option<AliasToolCallingDef> {
    effective_config().alias_tool_calling.get(alias).cloned()
}

/// Return every configured model-catalog entry, sorted by provider then id.
pub fn model_catalog_entries() -> Vec<(String, ModelDef)> {
    let config = effective_config();
    model_catalog_entries_with_config(&config)
}

pub(crate) fn model_catalog_entries_with_config(
    config: &ProvidersConfig,
) -> Vec<(String, ModelDef)> {
    sorted_model_entries_with_config(config)
        .into_iter()
        .map(|(id, model)| {
            let provider = model.provider.clone();
            (
                id.clone(),
                with_effective_capability_tags(id, provider, model),
            )
        })
        .collect()
}

pub(crate) fn sorted_model_entries_with_config(
    config: &ProvidersConfig,
) -> Vec<(String, ModelDef)> {
    let mut entries: Vec<_> = config
        .models
        .iter()
        .map(|(id, model)| (id.clone(), model.clone()))
        .collect();
    entries.sort_by(|(id_a, model_a), (id_b, model_b)| {
        model_a
            .provider
            .cmp(&model_b.provider)
            .then_with(|| id_a.cmp(id_b))
    });
    entries
}

pub fn model_catalog_entry(model_id: &str) -> Option<ModelDef> {
    effective_config()
        .models
        .get(model_id)
        .cloned()
        .map(|model| {
            let provider = model.provider.clone();
            with_effective_capability_tags(model_id.to_string(), provider, model)
        })
}

/// Return the collision-free catalog id for one concrete provider route.
///
/// Runtime transports use `wire_model`, while pricing and other catalog
/// metadata are keyed by the authored catalog id. Resolve either identity
/// without allowing an identically named model from another provider to win.
pub fn model_catalog_id_for_route(provider: &str, model_id: &str) -> Option<String> {
    let config = effective_config();
    let normalized_id = normalize_model_id(model_id);
    config
        .models
        .get_key_value(model_id)
        .filter(|(_, model)| model.provider == provider)
        .or_else(|| {
            config
                .models
                .get_key_value(&normalized_id)
                .filter(|(_, model)| model.provider == provider)
        })
        .or_else(|| {
            config.models.iter().find(|(_, model)| {
                model.provider == provider
                    && model
                        .wire_model
                        .as_deref()
                        .is_some_and(|wire| wire == model_id || wire == normalized_id.as_str())
            })
        })
        .map(|(id, _)| id.clone())
}

pub fn model_rate_limits(model_id: &str) -> Option<RateLimitsDef> {
    model_catalog_entry(model_id).and_then(|model| model.rate_limits)
}

/// Resolve a named model ladder declared under `[model_ladders.<name>]`.
/// Returns `None` when no ladder with that name exists in the effective
/// (base + overlay) catalog.
pub fn model_ladder(name: &str) -> Option<ModelLadderDef> {
    effective_config().model_ladders.get(name).cloned()
}

/// Sorted names of every declared model ladder — used to build a helpful
/// "did you mean" error when a `ladder:` option names an unknown ladder.
pub fn model_ladder_names() -> Vec<String> {
    effective_config().model_ladders.keys().cloned().collect()
}

pub fn wire_model_id(model_id: &str) -> String {
    model_catalog_entry(model_id)
        .and_then(|model| model.wire_model)
        .unwrap_or_else(|| model_id.to_string())
}

/// Resolve the model identity used by the capability matrix for one concrete
/// provider route without deriving capability tags (which would recurse back
/// into capability lookup). Collision-free catalog ids may differ from the
/// upstream creator/model slug that provider-family rules match.
pub(crate) fn capability_model_id(provider: &str, model_id: &str) -> String {
    if !provider_has_feature(provider, "wire_model_capabilities") {
        return model_id.to_string();
    }
    effective_config()
        .models
        .get(model_id)
        .filter(|model| model.provider == provider)
        .and_then(|model| model.wire_model.clone())
        .unwrap_or_else(|| model_id.to_string())
}

pub fn provider_rate_limits(provider: &str) -> Option<RateLimitsDef> {
    provider_config(provider).and_then(|provider| {
        provider
            .rate_limits
            .unwrap_or_default()
            .with_rpm_fallback(provider.rpm)
    })
}

pub fn model_equivalence_group(model_id: &str) -> Option<String> {
    model_catalog_entry(model_id).and_then(|model| {
        model
            .equivalence_group
            .or(model.logical_model)
            .filter(|group| !group.trim().is_empty())
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EquivalentModelRequirements {
    pub context_tokens: Option<u64>,
    pub native_tools: bool,
    pub text_tool_wire_format: bool,
    pub provider_tool_types: Vec<String>,
    pub vision: bool,
    pub url_images: bool,
    pub audio: bool,
    pub pdf: bool,
    pub video: bool,
    pub files_api: bool,
    pub thinking: bool,
    pub reasoning_effort: bool,
    pub structured_output: bool,
    pub structured_output_mode: Option<String>,
}

impl EquivalentModelRequirements {
    fn from_source_context(
        context_tokens: u64,
        caps: &crate::llm::capabilities::Capabilities,
    ) -> Self {
        Self {
            context_tokens: Some(context_tokens),
            native_tools: caps.native_tools,
            text_tool_wire_format: caps.text_tool_wire_format_supported,
            provider_tool_types: equivalent_provider_tool_types_for_capabilities(caps),
            vision: caps.vision_supported,
            url_images: caps.image_url_input_supported,
            audio: caps.audio,
            pdf: caps.pdf,
            video: caps.video,
            files_api: caps.files_api_supported,
            thinking: !caps.thinking_modes.is_empty(),
            reasoning_effort: caps.reasoning_effort_supported,
            structured_output: caps.structured_output.is_some(),
            structured_output_mode: Some(caps.structured_output_mode.clone()),
        }
    }
}

fn equivalent_provider_tool_types_for_capabilities(
    caps: &crate::llm::capabilities::Capabilities,
) -> Vec<String> {
    let mut kinds = caps.hosted_tools.clone();
    if caps.computer_use_style.is_some() {
        kinds.push("computer_use".to_string());
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

fn provider_tool_type_matches(
    caps: &crate::llm::capabilities::Capabilities,
    required: &str,
) -> bool {
    if required == "computer_use" && caps.computer_use_style.is_some() {
        return true;
    }
    caps.hosted_tools
        .iter()
        .any(|kind| kind == required || (required == "computer_use" && kind == "computer"))
}

/// Return same-logical-model routes that can be considered for explicit
/// failover or cross-provider experiments. Equivalence is a catalog assertion
/// about compatible model weights/family, not wire-level identity.
pub fn equivalent_model_catalog_entries_for_requirements(
    selector: &str,
    requirements: EquivalentModelRequirements,
) -> Vec<(String, ModelDef)> {
    let resolved = resolve_model_info(selector);
    let Some(group) = model_equivalence_group(&resolved.id) else {
        return Vec::new();
    };
    let config = effective_config();
    let Some(source) = config.models.get(&resolved.id) else {
        return Vec::new();
    };
    let source_context = source
        .runtime_context_window
        .unwrap_or(source.context_window);
    let minimum_context = requirements.context_tokens.unwrap_or(source_context);

    sorted_model_entries_with_config(&config)
        .into_iter()
        .filter(|(id, model)| !(id == &resolved.id && model.provider == resolved.provider))
        .filter(|(_, model)| !model.deprecated)
        .filter(|(_, model)| model.availability != ModelAvailability::Dedicated)
        .filter(|(_, model)| {
            model.equivalence_group.as_deref() == Some(group.as_str())
                || model.logical_model.as_deref() == Some(group.as_str())
        })
        .filter(|(id, model)| {
            let caps = crate::llm::capabilities::lookup(&model.provider, id);
            let candidate_context = model.runtime_context_window.unwrap_or(model.context_window);
            let context_matches = candidate_context >= minimum_context;
            let native_tools_match = !requirements.native_tools || caps.native_tools;
            let text_tool_format_match =
                !requirements.text_tool_wire_format || caps.text_tool_wire_format_supported;
            let provider_tools_match = requirements
                .provider_tool_types
                .iter()
                .all(|required| provider_tool_type_matches(&caps, required));
            let vision_match = !requirements.vision || caps.vision_supported;
            let url_images_match = !requirements.url_images
                || crate::llm::provider::provider_supports_image_urls(&model.provider, id);
            let audio_match = !requirements.audio || caps.audio;
            let pdf_match = !requirements.pdf || caps.pdf;
            let video_match = !requirements.video || caps.video;
            let files_api_match = !requirements.files_api || caps.files_api_supported;
            let thinking_match = !requirements.thinking || !caps.thinking_modes.is_empty();
            let reasoning_effort_match =
                !requirements.reasoning_effort || caps.reasoning_effort_supported;
            let structured_output_match =
                !requirements.structured_output || caps.structured_output.is_some();
            let structured_output_mode_match = requirements
                .structured_output_mode
                .as_ref()
                .is_none_or(|mode| mode == &caps.structured_output_mode);
            context_matches
                && native_tools_match
                && text_tool_format_match
                && provider_tools_match
                && vision_match
                && url_images_match
                && audio_match
                && pdf_match
                && video_match
                && files_api_match
                && thinking_match
                && reasoning_effort_match
                && structured_output_match
                && structured_output_mode_match
        })
        .map(|(id, model)| {
            let provider = model.provider.clone();
            (
                id.clone(),
                with_effective_capability_tags(id, provider, model),
            )
        })
        .collect()
}

/// Request-shaped equivalent routes: constrain the context window but only
/// require capabilities the current call actually resolved to use.
pub fn equivalent_model_catalog_entries_for_context(
    selector: &str,
    required_context_tokens: Option<u64>,
) -> Vec<(String, ModelDef)> {
    equivalent_model_catalog_entries_for_requirements(
        selector,
        EquivalentModelRequirements {
            context_tokens: required_context_tokens,
            ..EquivalentModelRequirements::default()
        },
    )
}

pub fn equivalent_model_catalog_entries(selector: &str) -> Vec<(String, ModelDef)> {
    let resolved = resolve_model_info(selector);
    let config = effective_config();
    let Some(source) = config.models.get(&resolved.id) else {
        return Vec::new();
    };
    let source_caps = crate::llm::capabilities::lookup(&source.provider, &resolved.id);
    let source_context = source
        .runtime_context_window
        .unwrap_or(source.context_window);
    equivalent_model_catalog_entries_for_requirements(
        selector,
        EquivalentModelRequirements::from_source_context(source_context, &source_caps),
    )
}

pub fn qc_default_model(provider: &str) -> Option<String> {
    std::env::var("BURIN_QC_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            effective_config()
                .qc_defaults
                .get(&provider.to_lowercase())
                .cloned()
        })
}

pub fn default_model_for_provider(provider: &str) -> String {
    if provider_uses_acp(provider) {
        return "default".to_string();
    }
    match provider {
        "local" => crate::test_env::env_var_seamed("LOCAL_LLM_MODEL")
            .or_else(|| crate::test_env::env_var_seamed("HARN_LLM_MODEL"))
            .unwrap_or_else(|| "gemma-4-26b-a4b-it".to_string()),
        "mlx" => std::env::var("MLX_MODEL_ID")
            .unwrap_or_else(|_| "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit".to_string()),
        "openai" => "gpt-4o-mini".to_string(),
        "ollama" => "llama3.2".to_string(),
        "openrouter" => "anthropic/claude-sonnet-4.6".to_string(),
        _ => "claude-sonnet-4-6".to_string(),
    }
}

pub fn qc_defaults() -> BTreeMap<String, String> {
    effective_config().qc_defaults.clone()
}

pub fn model_pricing_per_mtok(model_id: &str) -> Option<ModelPricing> {
    effective_config()
        .models
        .get(model_id)
        .and_then(|model| model.pricing.clone())
}

pub fn model_pricing_per_mtok_for_route(provider: &str, model_id: &str) -> Option<ModelPricing> {
    let catalog_id = model_catalog_id_for_route(provider, model_id)?;
    model_pricing_per_mtok(&catalog_id)
}

/// Per-MTok whole-request pricing selected for the provider-reported input
/// usage. Models without input-token bands retain their base rates.
pub fn model_pricing_for_input_tokens(model_id: &str, input_tokens: i64) -> Option<ModelPricing> {
    model_pricing_per_mtok(model_id).map(|pricing| pricing.for_input_tokens(input_tokens))
}

pub fn model_pricing_for_route_input_tokens(
    provider: &str,
    model_id: &str,
    input_tokens: i64,
) -> Option<ModelPricing> {
    model_pricing_per_mtok_for_route(provider, model_id)
        .map(|pricing| pricing.for_input_tokens(input_tokens))
}

/// Per-MTok pricing for a named serving tier, when the catalog declares one.
/// Returns `None` for models with no matching tier or a tier that omits
/// explicit pricing — callers fall back to standard pricing in that case.
pub fn model_serving_tier_pricing_per_mtok(model_id: &str, tier_id: &str) -> Option<ModelPricing> {
    effective_config()
        .models
        .get(model_id)
        .and_then(|model| model.serving_tiers.iter().find(|tier| tier.id == tier_id))
        .and_then(|tier| tier.pricing.clone())
}

pub fn model_serving_tier_pricing_per_mtok_for_route(
    provider: &str,
    model_id: &str,
    tier_id: &str,
) -> Option<ModelPricing> {
    let catalog_id = model_catalog_id_for_route(provider, model_id)?;
    model_serving_tier_pricing_per_mtok(&catalog_id, tier_id)
}

pub fn pricing_per_1k_for(provider: &str, model_id: &str) -> Option<(f64, f64)> {
    model_pricing_per_mtok_for_route(provider, model_id)
        .map(|pricing| {
            (
                pricing.input_per_mtok / 1000.0,
                pricing.output_per_mtok / 1000.0,
            )
        })
        .or_else(|| {
            let (input, output, _) = provider_economics(provider);
            match (input, output) {
                (Some(input), Some(output)) => Some((input, output)),
                _ => None,
            }
        })
}

pub fn auth_env_names(auth_env: &AuthEnv) -> Vec<String> {
    match auth_env {
        AuthEnv::None => Vec::new(),
        AuthEnv::Single(name) => vec![name.clone()],
        AuthEnv::Multiple(names) => names.clone(),
    }
}

/// Check if a provider advertises a legacy provider-level feature.
pub fn provider_has_feature(provider: &str, feature: &str) -> bool {
    provider_config(provider)
        .map(|p| p.features.iter().any(|f| f == feature))
        .unwrap_or(false)
}

/// Provider-level catalog pricing/latency. Model-specific catalog pricing
/// wins when available; this is the adapter-level fallback used by routing
/// and portal summaries when a model has no explicit catalog entry.
pub fn provider_economics(provider: &str) -> (Option<f64>, Option<f64>, Option<u64>) {
    provider_config(provider)
        .map(|p| (p.cost_per_1k_in, p.cost_per_1k_out, p.latency_p50_ms))
        .unwrap_or((None, None, None))
}

/// The tool-call channel a `tool_format` string addresses.
///
/// `native` is the provider JSON tool-calling channel; `text` (the canonical
/// tagged/heredoc grammar) and `json` (fenced-JSON) are both TEXT-channel
/// formats — they ride in the assistant's visible content and parse with a
/// text parser. This is the single source of truth for "is this format a
/// text-channel format?" so the parity gates, native-tools resolution, and
/// tool-result message role all agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFormatChannel {
    /// Provider native JSON tool calling.
    Native,
    /// A text-channel grammar carried in assistant content (`text` or `json`).
    Text,
}

/// Classify a `tool_format` string into its channel, or `None` for an unknown
/// value (a typo, or a not-yet-wired format). Callers use this to reject
/// unknown formats loudly instead of silently defaulting.
///
/// EXHAUSTIVE-MATCH GUARD: this `match` is the canonical place tool_format is
/// switched. Adding a new format requires a branch here, so a half-wired
/// format fails to compile rather than silently reading as text.
pub fn tool_format_channel(format: &str) -> Option<ToolFormatChannel> {
    match format {
        "native" => Some(ToolFormatChannel::Native),
        // `adaptive` is an opt-in permissive text-channel union (DEFAULT-OFF:
        // no route resolves to it; reachable only via an explicit pin/request).
        "text" | "json" | "adaptive" => Some(ToolFormatChannel::Text),
        _ => None,
    }
}

/// True when `format` is a tool_format Harn understands (`native`, `text`, or
/// `json`). Used to gate the capability-matrix `preferred_tool_format` so a
/// pinned format is honored, while an unknown value falls through to the
/// native/text heuristic.
pub fn is_known_tool_format(format: &str) -> bool {
    tool_format_channel(format).is_some()
}

/// Resolve the default tool format for a model+provider combination.
/// Priority: alias `tool_format` (matched by model ID) > provider/model
/// capability matrix > legacy provider feature > "json" (the global
/// text-channel default; heredoc "text" is opt-in via a pin or explicit
/// request).
pub fn default_tool_format(model: &str, provider: &str) -> String {
    let config = effective_config();
    default_tool_format_with_config(&config, model, provider)
}

pub(crate) fn default_tool_format_with_config(
    config: &ProvidersConfig,
    model: &str,
    provider: &str,
) -> String {
    // Aliases match by model ID + provider, or by alias name.
    for (name, alias) in &config.aliases {
        let matches = (alias.id == model && alias.provider == provider) || name == model;
        if matches {
            if let Some(ref fmt) = alias.tool_format {
                return fmt.clone();
            }
        }
    }
    let capabilities = crate::llm::capabilities::lookup(provider, model);
    if let Some(format) = capabilities.preferred_tool_format.as_deref() {
        // A capability row may pin any known tool_format, including `text`
        // (heredoc) — the reverse safety valve a regressing model uses to pin
        // OFF the global json default. `json` is also honored when a row sets
        // it. The exhaustive match below is the EXHAUSTIVE-MATCH GUARD: a new
        // tool_format that isn't classified here fails loudly rather than
        // silently falling through to the native/json heuristic.
        if is_known_tool_format(format) {
            return format.to_string();
        }
    }
    let capability_matrix_native = capabilities.native_tools;
    let legacy_provider_native = config
        .providers
        .get(provider)
        .map(|p| p.features.iter().any(|f| f == "native_tools"))
        .unwrap_or(false);
    if capability_matrix_native || legacy_provider_native {
        "native".to_string()
    } else {
        // GLOBAL DEFAULT: a text-channel model with no pinned format resolves
        // to fenced-json (`json`), not heredoc (`text`). The win is STRUCTURAL
        // — a JSON string can't carry a raw newline, so a `<<EOF` content
        // delimiter never collides with the call wrapper (heredoc's known
        // production defect: models leak `<<EOF` into file content → the
        // `line 0: <<` thrash). Fenced-json swept a clean 1.0/1.0/1.0
        // (compliance/parse-determinism/expressiveness) across every model
        // measured, and the structural guarantee generalizes to unmeasured
        // models. Heredoc (`text`) stays selectable explicitly and via a
        // per-model `preferred_tool_format = "text"` pin (the reverse valve).
        "json".to_string()
    }
}

fn with_effective_capability_tags(
    model_id: String,
    provider: String,
    mut model: ModelDef,
) -> ModelDef {
    model.capabilities = effective_model_capability_tags(&provider, &model_id);
    model
}

/// Legacy display tags derived from the canonical provider/model capability
/// matrix. The matrix is the source of truth; `models.*.capabilities` in
/// providers.toml is accepted only for backwards-compatible parsing.
pub fn effective_model_capability_tags(provider: &str, model_id: &str) -> Vec<String> {
    let caps = crate::llm::capabilities::lookup(provider, model_id);
    let mut tags = capability_tags_from_capabilities(&caps);
    if effective_batch_api_supported(provider, &caps) && !tags.iter().any(|tag| tag == "batch") {
        tags.push("batch".to_string());
    }
    tags
}

pub fn effective_batch_api_supported(
    provider: &str,
    caps: &crate::llm::capabilities::Capabilities,
) -> bool {
    caps.batch_api || provider_has_feature(provider, "batch")
}

pub(crate) fn capability_tags_from_capabilities(
    caps: &crate::llm::capabilities::Capabilities,
) -> Vec<String> {
    let mut tags = Vec::new();
    // Today all Harn chat providers expose streaming. Keep this as a
    // transport baseline rather than a duplicated per-model declaration.
    tags.push("streaming".to_string());
    if caps.native_tools || caps.text_tool_wire_format_supported {
        tags.push("tools".to_string());
    }
    if !caps.tool_search.is_empty() {
        tags.push("tool_search".to_string());
    }
    if caps.vision || caps.vision_supported {
        tags.push("vision".to_string());
    }
    if caps.audio {
        tags.push("audio".to_string());
    }
    if caps.pdf {
        tags.push("pdf".to_string());
    }
    if caps.video {
        tags.push("video".to_string());
    }
    if caps.files_api_supported {
        tags.push("files".to_string());
    }
    if caps.batch_api {
        tags.push("batch".to_string());
    }
    if caps.prompt_caching {
        tags.push("prompt_caching".to_string());
    }
    if !caps.thinking_modes.is_empty() {
        tags.push("thinking".to_string());
    }
    if caps.interleaved_thinking_supported
        || caps
            .thinking_modes
            .iter()
            .any(|mode| mode == "adaptive" || mode == "effort")
    {
        tags.push("extended_thinking".to_string());
    }
    if caps.structured_output.is_some() || caps.json_schema.is_some() {
        tags.push("structured_output".to_string());
    }
    tags
}

/// Resolve a tier or alias into a concrete model/provider pair.
pub fn resolve_tier_model(
    target: &str,
    preferred_provider: Option<&str>,
) -> Option<(String, String)> {
    let config = effective_config();

    let candidate_aliases = if let Some(provider) = preferred_provider {
        vec![
            format!("{provider}/{target}"),
            format!("{provider}:{target}"),
            format!("tier/{target}"),
            target.to_string(),
        ]
    } else {
        vec![format!("tier/{target}"), target.to_string()]
    };

    for alias_name in candidate_aliases {
        if let Some(alias) = config.aliases.get(&alias_name) {
            return Some((alias.id.clone(), alias.provider.clone()));
        }
    }

    None
}

/// Return all configured alias-backed model/provider pairs whose resolved
/// model falls into the requested capability tier. The result is de-duplicated
/// and sorted deterministically by provider then model id.
pub fn tier_candidates(target: &str) -> Vec<(String, String)> {
    let config = effective_config();
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();

    for alias in config.aliases.values() {
        let pair = (alias.id.clone(), alias.provider.clone());
        if seen.contains(&pair) {
            continue;
        }
        if model_tier(&alias.id) == target {
            seen.insert(pair.clone());
            candidates.push(pair);
        }
    }

    candidates.sort_by(|(model_a, provider_a), (model_b, provider_b)| {
        provider_a
            .cmp(provider_b)
            .then_with(|| model_a.cmp(model_b))
    });
    candidates
}

/// Return all configured alias-backed model/provider pairs. Used by routing
/// policies that need to compare alternatives across tiers.
pub fn all_model_candidates() -> Vec<(String, String)> {
    let config = effective_config();
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();

    for alias in config.aliases.values() {
        let pair = (alias.id.clone(), alias.provider.clone());
        if seen.insert(pair.clone()) {
            candidates.push(pair);
        }
    }

    candidates.sort_by(|(model_a, provider_a), (model_b, provider_b)| {
        provider_a
            .cmp(provider_b)
            .then_with(|| model_a.cmp(model_b))
    });
    candidates
}
