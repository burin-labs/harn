//! Generated provider/model catalog artifact support.
//!
//! `llm_config` is the runtime source of truth. This module projects that
//! configuration plus the capability matrix into a stable JSON contract that
//! downstream products can vendor without re-parsing Harn's internal TOML or
//! Rust literals.

use std::collections::{BTreeMap, BTreeSet};

use crate::llm;
use crate::llm_config::{
    self, AliasDef, AliasToolCallingDef, HealthcheckDef, LocalMemoryDef, ModelArchitectureDef,
    ModelDef, ModelPricing, ProviderDef, RateLimitsDef, ServingPerformanceDef,
};
use chrono::{NaiveDate, Utc};

pub const PROVIDER_CATALOG_SCHEMA_VERSION: u32 = 4;
pub const PROVIDER_CATALOG_SCHEMA_ID: &str =
    "https://harnlang.com/schemas/provider-catalog.v4.json";
pub const PROVIDER_CATALOG_GENERATOR: &str = "harn provider catalog export";
pub const HARN_DISABLE_CATALOG_REFRESH_ENV: &str = "HARN_DISABLE_CATALOG_REFRESH";
pub const HARN_PROVIDER_CATALOG_URL_ENV: &str = "HARN_PROVIDER_CATALOG_URL";
pub const HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV: &str = "HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED";
pub const HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV: &str = "HARN_PROVIDER_CATALOG_TRUSTED_KEYS";
pub const DEFAULT_PROVIDER_CATALOG_URL: &str =
    "https://harnlang.com/provider-catalog/provider-catalog.json";

const DEFAULT_REMOTE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const REMOTE_CACHE_DIR: &str = "provider-catalog";
const REMOTE_CACHE_BODY_FILE: &str = "catalog.json";
const REMOTE_CACHE_META_FILE: &str = "catalog.meta.json";
const FACT_FRESHNESS_WARNING_DAYS: i64 = 180;

mod bindings;
mod remote;
mod schema;
#[cfg(test)]
mod tests;
mod types;
mod validation;

pub use bindings::{swift_binding, swift_binding_embedded, typescript_declarations};
pub use remote::{refresh_runtime_catalog, CatalogRefreshOptions, CatalogRefreshReport};
pub use schema::{schema_json, schema_value};
pub use types::*;
pub use validation::{validate_artifact, validate_current};

fn config_from_artifact(artifact: &ProviderCatalogArtifact) -> llm_config::ProvidersConfig {
    llm_config::ProvidersConfig {
        providers: artifact
            .providers
            .iter()
            .map(|provider| (provider.id.clone(), provider_def_from_catalog(provider)))
            .collect(),
        aliases: artifact
            .aliases
            .iter()
            .map(|alias| {
                (
                    alias.name.clone(),
                    llm_config::AliasDef {
                        id: alias.model_id.clone(),
                        provider: alias.provider.clone(),
                        tool_format: alias.tool_format.clone(),
                    },
                )
            })
            .collect(),
        alias_tool_calling: artifact
            .aliases
            .iter()
            .filter_map(|alias| {
                alias
                    .tool_calling
                    .clone()
                    .map(|tool_calling| (alias.name.clone(), tool_calling))
            })
            .collect(),
        models: artifact
            .models
            .iter()
            .map(|model| (model.id.clone(), model_def_from_catalog(model)))
            .collect(),
        qc_defaults: artifact.qc_defaults.clone(),
        ..llm_config::ProvidersConfig::default()
    }
}

fn provider_def_from_catalog(provider: &CatalogProvider) -> llm_config::ProviderDef {
    llm_config::ProviderDef {
        display_name: Some(provider.display_name.clone()),
        icon: provider.icon.clone(),
        base_url: provider.endpoint.base_url.clone(),
        base_url_env: provider.endpoint.base_url_env.clone(),
        region_env: provider.endpoint.region_env.clone(),
        regions: provider
            .endpoint
            .regions
            .iter()
            .map(|(id, region)| {
                (
                    id.clone(),
                    llm_config::ProviderRegionDef {
                        base_url: region.base_url.clone(),
                        label: region.label.clone(),
                        source_url: region.source_url.clone(),
                        last_verified: region.last_verified.clone(),
                        notes: region.notes.clone(),
                    },
                )
            })
            .collect(),
        auth_style: provider.auth.style.clone(),
        auth_style_explicit: true,
        auth_header: provider.auth.header.clone(),
        auth_env: match provider.auth.env.as_slice() {
            [] => llm_config::AuthEnv::None,
            [one] => llm_config::AuthEnv::Single(one.clone()),
            many => llm_config::AuthEnv::Multiple(many.to_vec()),
        },
        extra_headers: provider.extra_headers.clone(),
        chat_endpoint: provider.endpoint.chat_endpoint.clone(),
        completion_endpoint: provider.endpoint.completion_endpoint.clone(),
        healthcheck: provider
            .healthcheck
            .clone()
            .map(healthcheck_def_from_catalog),
        features: provider.features.clone(),
        rpm: provider.rpm,
        rate_limits: provider.rate_limits.clone(),
        local_runtime: provider.local_runtime.clone(),
        latency_p50_ms: provider.latency_p50_ms,
        performance: provider.performance.clone(),
        ..llm_config::ProviderDef::default()
    }
}

fn model_def_from_catalog(model: &CatalogModel) -> llm_config::ModelDef {
    llm_config::ModelDef {
        name: model.name.clone(),
        provider: model.provider.clone(),
        context_window: model.context_window,
        logical_model: model.logical_model.clone(),
        equivalence_group: model.equivalence_group.clone(),
        served_variant: model.served_variant.clone(),
        wire_model: model.wire_model.clone(),
        api_dialect: model.api_dialect.clone(),
        rate_limits: model.rate_limits.clone(),
        performance: model.performance.clone(),
        architecture: model.architecture.clone(),
        local_memory: model.local_memory.clone(),
        runtime_context_window: model.runtime_context_window,
        stream_timeout: model.stream_timeout,
        capabilities: model.capability_tags.clone(),
        pricing: model.pricing.clone(),
        deprecated: model.deprecation.status == DeprecationStatus::Deprecated,
        deprecation_note: model.deprecation.note.clone(),
        superseded_by: model.deprecation.superseded_by.clone(),
        serving_tiers: model.serving_tiers.clone(),
        quality_tags: model.quality_tags.clone(),
        availability: match model.availability {
            ModelAvailabilityStatus::Serverless => llm_config::ModelAvailability::Serverless,
            ModelAvailabilityStatus::Dedicated => llm_config::ModelAvailability::Dedicated,
            ModelAvailabilityStatus::Unknown => llm_config::ModelAvailability::Unknown,
        },
        tier: Some(model.tier.clone()),
        open_weight: model.open_weight,
        strengths: model.strengths.clone(),
        benchmarks: model.benchmarks.clone(),
        family: Some(model.family.clone()),
        lineage: Some(model.lineage.clone()),
        complementary_with: model.complementary_with.clone(),
        avoid_as_reviewer_for: model.avoid_as_reviewer_for.clone(),
    }
}

pub fn artifact() -> ProviderCatalogArtifact {
    let config = llm_config::effective_config();
    artifact_from_config(&config, CatalogCapabilityOverrides::CurrentThread)
}

/// Build the catalog artifact hermetically: from the compiled-in embedded
/// provider config and embedded capability matrix only, ignoring the
/// developer's `~/.config/harn/providers.toml`, environment overrides
/// (`HARN_PROVIDERS_CONFIG`, `HARN_DEFAULT_PROVIDER`, `HARN_LLM_*`), the
/// process runtime-catalog overlay, and any thread-local user overrides.
///
/// This is what `harn provider catalog export` / `provider catalog validate
/// --check-artifacts` use so the checked-in `spec/provider-catalog/*` artifacts
/// are a pure function of the source tree. Do NOT use this for live runtime
/// catalog presentation — use [`artifact`] / [`artifact_with_overrides`] there,
/// which legitimately reflect the host's configuration.
///
/// `explicit_overlay` is an optional declared overlay (e.g. a `--overlay` file
/// named on the command line); it is reproducible input, not ambient machine
/// state, so it is merged on top of the embedded base while staying hermetic.
/// `explicit_capabilities` is the matching declared capability overlay (e.g. a
/// `--capabilities-overlay` file): without one, only the built-in capability
/// matrix is consulted — never the thread-local capability user-overrides the
/// CLI may have installed.
pub fn artifact_embedded(
    explicit_overlay: Option<&llm_config::ProvidersConfig>,
    explicit_capabilities: Option<&llm::capabilities::CapabilitiesFile>,
) -> ProviderCatalogArtifact {
    let config = llm_config::embedded_config(explicit_overlay);
    artifact_from_config(
        &config,
        CatalogCapabilityOverrides::Explicit(explicit_capabilities),
    )
}

/// Build a catalog artifact for a runtime that has captured explicit provider
/// and capability overlays. `None` means no override for that layer; unlike
/// `artifact()`, this does not read thread-local user overrides implicitly.
pub fn artifact_with_overrides(
    llm_config_overrides: Option<&llm_config::ProvidersConfig>,
    llm_capability_overrides: Option<&llm::capabilities::CapabilitiesFile>,
) -> ProviderCatalogArtifact {
    let config = llm_config::effective_config_with_user_overrides(llm_config_overrides);
    artifact_from_config(
        &config,
        CatalogCapabilityOverrides::Explicit(llm_capability_overrides),
    )
}

#[derive(Clone, Copy)]
enum CatalogCapabilityOverrides<'a> {
    CurrentThread,
    Explicit(Option<&'a llm::capabilities::CapabilitiesFile>),
}

fn artifact_from_config(
    config: &llm_config::ProvidersConfig,
    llm_capability_overrides: CatalogCapabilityOverrides<'_>,
) -> ProviderCatalogArtifact {
    // Suppression filters the presentation artifact only: model rows, their
    // aliases, and (because variants are derived from the surviving rows
    // below) any recommendation variant that pointed at a suppressed route.
    let suppressed = suppressed_routes(config);
    let alias_entries = config
        .aliases
        .iter()
        .filter(|(_, alias)| !is_suppressed(&suppressed, &alias.provider, &alias.id))
        .map(|(name, alias)| (name.clone(), alias.clone()))
        .collect::<Vec<_>>();
    let aliases_by_model = aliases_by_model(&alias_entries);
    let providers: Vec<_> = config
        .providers
        .iter()
        .map(|(id, provider)| catalog_provider(id.clone(), provider.clone()))
        .collect();
    let models = llm_config::sorted_model_entries_with_config(config)
        .into_iter()
        .filter(|(id, model)| !is_suppressed(&suppressed, &model.provider, id))
        .map(|(id, model)| {
            catalog_model(
                id,
                model,
                &aliases_by_model,
                config,
                llm_capability_overrides,
            )
        })
        .collect::<Vec<_>>();
    let aliases = alias_entries
        .iter()
        .map(|(name, alias)| {
            catalog_alias(name, alias, config.alias_tool_calling.get(name).cloned())
        })
        .collect::<Vec<_>>();
    let variants = catalog_variants(&models, &aliases);
    let routing_routes = catalog_routing_routes(&models, &providers);

    ProviderCatalogArtifact {
        schema_version: PROVIDER_CATALOG_SCHEMA_VERSION,
        schema: PROVIDER_CATALOG_SCHEMA_ID.to_string(),
        generated_by: PROVIDER_CATALOG_GENERATOR.to_string(),
        providers,
        models,
        aliases,
        variants,
        routing_routes,
        qc_defaults: config.qc_defaults.clone(),
    }
}

pub fn artifact_json() -> Result<String, serde_json::Error> {
    artifact_to_json(&artifact())
}

/// Hermetic counterpart of [`artifact_json`] for generating the checked-in
/// `provider-catalog.json` artifact. See [`artifact_embedded`].
pub fn artifact_json_embedded(
    explicit_overlay: Option<&llm_config::ProvidersConfig>,
    explicit_capabilities: Option<&llm::capabilities::CapabilitiesFile>,
) -> Result<String, serde_json::Error> {
    artifact_to_json(&artifact_embedded(explicit_overlay, explicit_capabilities))
}

/// `[suppress]` route selectors parsed as `(provider, model_id)` pairs.
/// Split on the first colon only: model ids may themselves contain colons
/// (e.g. Ollama image tags). Entries without a colon match nothing.
fn suppressed_routes(config: &llm_config::ProvidersConfig) -> Vec<(String, String)> {
    config
        .suppress
        .routes
        .iter()
        .filter_map(|route| route.split_once(':'))
        .map(|(provider, model_id)| (provider.to_string(), model_id.to_string()))
        .collect()
}

fn is_suppressed(suppressed: &[(String, String)], provider: &str, model_id: &str) -> bool {
    suppressed
        .iter()
        .any(|(candidate_provider, candidate_model)| {
            candidate_provider == provider && candidate_model == model_id
        })
}

fn artifact_to_json(artifact: &ProviderCatalogArtifact) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(artifact).map(|mut text| {
        text.push('\n');
        text
    })
}

fn catalog_provider(id: String, provider: ProviderDef) -> CatalogProvider {
    CatalogProvider {
        display_name: provider
            .display_name
            .clone()
            .unwrap_or_else(|| title_case(&id)),
        icon: provider.icon.clone(),
        classification: provider_classification(&provider),
        endpoint: ProviderEndpoint {
            base_url: provider.base_url.clone(),
            base_url_env: provider.base_url_env.clone(),
            region_env: provider.region_env.clone(),
            regions: provider
                .regions
                .iter()
                .map(|(id, region)| {
                    (
                        id.clone(),
                        ProviderEndpointRegion {
                            base_url: region.base_url.clone(),
                            label: region.label.clone(),
                            source_url: region.source_url.clone(),
                            last_verified: region.last_verified.clone(),
                            notes: region.notes.clone(),
                        },
                    )
                })
                .collect(),
            chat_endpoint: provider.chat_endpoint.clone(),
            completion_endpoint: provider.completion_endpoint.clone(),
        },
        auth: ProviderAuth {
            style: provider.auth_style.clone(),
            header: provider.auth_header.clone(),
            env: llm_config::auth_env_names(&provider.auth_env),
            required: provider.auth_style != "none",
        },
        extra_headers: provider.extra_headers.clone(),
        healthcheck: provider
            .healthcheck
            .clone()
            .map(catalog_provider_healthcheck),
        protocols: provider_protocols(&id, &provider),
        features: provider.features.clone(),
        caveats: provider_caveats(&id, &provider),
        rpm: provider.rpm,
        rate_limits: provider
            .rate_limits
            .clone()
            .unwrap_or_default()
            .with_rpm_fallback(provider.rpm),
        local_runtime: provider.local_runtime.clone(),
        latency_p50_ms: provider.latency_p50_ms,
        performance: provider
            .performance
            .clone()
            .filter(|performance| !performance.is_empty()),
        id,
    }
}

fn catalog_routing_routes(
    models: &[CatalogModel],
    providers: &[CatalogProvider],
) -> Vec<CatalogRoutingRoute> {
    let providers_by_id = providers
        .iter()
        .map(|provider| (provider.id.as_str(), provider))
        .collect::<BTreeMap<_, _>>();
    models
        .iter()
        .filter(|model| model.deprecation.status == DeprecationStatus::Active)
        .filter_map(|model| {
            let provider = providers_by_id.get(model.provider.as_str())?;
            Some(CatalogRoutingRoute {
                provider: model.provider.clone(),
                model: model.wire_model.clone().unwrap_or_else(|| model.id.clone()),
                base_url: Some(provider.endpoint.base_url.clone()),
                secret_env: provider.auth.env.first().cloned(),
                timeout_ms: model.stream_timeout.and_then(stream_timeout_ms),
                label: Some(model.name.clone()),
                family: Some(model.family.clone()),
                capabilities: routing_capabilities(model),
            })
        })
        .collect()
}

fn stream_timeout_ms(seconds: f64) -> Option<u64> {
    if seconds.is_finite() && seconds > 0.0 {
        Some((seconds * 1000.0).ceil() as u64)
    } else {
        None
    }
}

fn routing_capabilities(model: &CatalogModel) -> Vec<String> {
    let mut capabilities = model
        .capability_tags
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if model.tool_support.native {
        capabilities.insert("native_tools".to_string());
    }
    if model.tool_support.text {
        capabilities.insert("text_tools".to_string());
    }
    if model.prompt_cache {
        capabilities.insert("prompt_caching".to_string());
    }
    if model.reasoning.effort_supported {
        capabilities.insert("reasoning_effort".to_string());
    }
    if model.reasoning.interleaved_supported {
        capabilities.insert("interleaved_thinking".to_string());
    }
    if model.format_preferences.prefers_markdown_scaffolding {
        capabilities.insert("prefers_markdown_scaffolding".to_string());
    }
    if model.format_preferences.prefers_xml_scaffolding {
        capabilities.insert("prefers_xml_scaffolding".to_string());
    }
    capabilities.insert(format!(
        "structured_output_mode:{}",
        model.format_preferences.structured_output_mode
    ));
    capabilities.into_iter().collect()
}

fn catalog_provider_healthcheck(healthcheck: HealthcheckDef) -> CatalogProviderHealthcheck {
    CatalogProviderHealthcheck {
        method: healthcheck.method,
        path: healthcheck.path,
        url: healthcheck.url,
        body: healthcheck.body,
    }
}

fn healthcheck_def_from_catalog(healthcheck: CatalogProviderHealthcheck) -> HealthcheckDef {
    HealthcheckDef {
        method: healthcheck.method,
        path: healthcheck.path,
        url: healthcheck.url,
        body: healthcheck.body,
    }
}

fn catalog_alias(
    name: &str,
    alias: &AliasDef,
    tool_calling: Option<AliasToolCallingDef>,
) -> CatalogAlias {
    CatalogAlias {
        name: name.to_string(),
        model_id: alias.id.clone(),
        provider: alias.provider.clone(),
        tool_format: alias.tool_format.clone(),
        tool_calling,
    }
}

fn catalog_model(
    id: String,
    model: ModelDef,
    aliases_by_model: &BTreeMap<(String, String), Vec<String>>,
    config: &llm_config::ProvidersConfig,
    llm_capability_overrides: CatalogCapabilityOverrides<'_>,
) -> CatalogModel {
    let caps = match llm_capability_overrides {
        CatalogCapabilityOverrides::CurrentThread => {
            llm::capabilities::lookup(&model.provider, &id)
        }
        CatalogCapabilityOverrides::Explicit(overrides) => {
            llm::capabilities::lookup_with_user_overrides(&model.provider, &id, overrides)
        }
    };
    let structured_output = caps
        .structured_output
        .clone()
        .or_else(|| caps.json_schema.clone())
        .unwrap_or_else(|| "none".to_string());
    let aliases = aliases_by_model
        .get(&(model.provider.clone(), id.clone()))
        .cloned()
        .unwrap_or_default();
    let quality_tags = model_quality_tags(&model, &aliases);
    let batch_api = effective_batch_api_supported_for_config(config, &model.provider, &caps);
    let mut capability_tags = llm_config::capability_tags_from_capabilities(&caps);
    if batch_api && !capability_tags.iter().any(|tag| tag == "batch") {
        capability_tags.push("batch".to_string());
    }
    let batch = catalog_batch_support(batch_api, &caps);
    CatalogModel {
        aliases,
        logical_model: model.logical_model.clone(),
        equivalence_group: model.equivalence_group.clone(),
        served_variant: model.served_variant.clone(),
        wire_model: model.wire_model.clone(),
        api_dialect: model.api_dialect.clone(),
        rate_limits: model
            .rate_limits
            .clone()
            .filter(|limits| !limits.is_empty()),
        performance: model
            .performance
            .clone()
            .filter(|performance| !performance.is_empty()),
        architecture: model
            .architecture
            .clone()
            .filter(|architecture| !architecture.is_empty()),
        local_memory: model
            .local_memory
            .clone()
            .filter(|memory| !memory.is_empty()),
        modalities: modalities_from_caps(&caps),
        tool_support: ModelToolSupport {
            native: caps.native_tools,
            text: caps.text_tool_wire_format_supported,
            preferred_format: caps.preferred_tool_format.clone(),
            parity: caps.tool_mode_parity.clone(),
            parity_notes: caps.tool_mode_parity_notes.clone(),
            empirical_parity: None,
            tool_search: caps.tool_search.clone(),
            max_tools: caps.max_tools,
        },
        structured_output,
        format_preferences: ModelFormatPreferences {
            prefers_xml_scaffolding: caps.prefers_xml_scaffolding,
            prefers_markdown_scaffolding: caps.prefers_markdown_scaffolding,
            structured_output_mode: caps.structured_output_mode.clone(),
            supports_assistant_prefill: caps.supports_assistant_prefill,
            prefers_role_developer: caps.prefers_role_developer,
            prefers_xml_tools: caps.prefers_xml_tools,
            thinking_block_style: caps.thinking_block_style.clone(),
        },
        reasoning: ModelReasoning {
            modes: caps.thinking_modes.clone(),
            effort_supported: caps.reasoning_effort_supported,
            none_supported: caps.reasoning_none_supported,
            interleaved_supported: caps.interleaved_thinking_supported,
            preserve_thinking: caps.preserve_thinking,
        },
        prompt_cache: caps.prompt_caching,
        batch,
        pricing: model.pricing.clone(),
        deprecation: ModelDeprecation {
            status: if model.deprecated {
                DeprecationStatus::Deprecated
            } else {
                DeprecationStatus::Active
            },
            note: model.deprecation_note.clone(),
            superseded_by: model.superseded_by.clone(),
        },
        availability: ModelAvailabilityStatus::from(model.availability),
        quality_tags,
        capability_tags,
        family: llm_config::model_family_with_config(config, &model.provider, &id),
        lineage: llm_config::model_lineage_with_config(config, &model.provider, &id),
        complementary_with: model.complementary_with.clone(),
        avoid_as_reviewer_for: model.avoid_as_reviewer_for.clone(),
        tier: llm_config::model_tier_with_config(config, &id),
        open_weight: model.open_weight,
        strengths: model.strengths.clone(),
        benchmarks: model.benchmarks.clone(),
        serving_tiers: model.serving_tiers.clone(),
        id,
        name: model.name,
        provider: model.provider,
        context_window: model.context_window,
        runtime_context_window: model.runtime_context_window,
        stream_timeout: model.stream_timeout,
    }
}

fn effective_batch_api_supported_for_config(
    config: &llm_config::ProvidersConfig,
    provider: &str,
    caps: &llm::capabilities::Capabilities,
) -> bool {
    caps.batch_api
        || config
            .providers
            .get(provider)
            .is_some_and(|provider| provider.features.iter().any(|feature| feature == "batch"))
}

fn catalog_batch_support(
    batch_api: bool,
    caps: &llm::capabilities::Capabilities,
) -> Option<ModelBatchSupport> {
    if !batch_api {
        return None;
    }
    Some(ModelBatchSupport {
        wire_format: caps.batch_wire_format.clone()?,
        input_mode: caps.batch_input_mode.clone()?,
        result_ordering: batch_result_ordering(caps.batch_result_ordering.as_deref()),
        partial_failure: batch_partial_failure(caps.batch_partial_failure.as_deref()),
        cancellation: batch_cancellation(caps.batch_cancellation.as_deref()),
        discount_percent: caps.batch_discount_percent,
        turnaround_hours: caps.batch_turnaround_hours,
        max_requests: caps.batch_max_requests,
        max_input_bytes: caps.batch_max_input_bytes,
        result_retention_days: caps.batch_result_retention_days,
        security_notes: caps.batch_security_notes.clone(),
        operational_notes: caps.batch_operational_notes.clone(),
    })
}

fn batch_result_ordering(value: Option<&str>) -> BatchResultOrdering {
    match value {
        Some("custom_id_rejoin") => BatchResultOrdering::CustomIdRejoin,
        Some("provider_ordered") => BatchResultOrdering::ProviderOrdered,
        _ => BatchResultOrdering::Unknown,
    }
}

fn batch_partial_failure(value: Option<&str>) -> BatchPartialFailure {
    match value {
        Some("per_request") => BatchPartialFailure::PerRequest,
        Some("whole_batch") => BatchPartialFailure::WholeBatch,
        _ => BatchPartialFailure::Unknown,
    }
}

fn batch_cancellation(value: Option<&str>) -> BatchCancellationSupport {
    match value {
        Some("supported") => BatchCancellationSupport::Supported,
        Some("not_supported") => BatchCancellationSupport::NotSupported,
        _ => BatchCancellationSupport::Unknown,
    }
}

fn model_quality_tags(model: &ModelDef, aliases: &[String]) -> Vec<String> {
    let mut tags: BTreeSet<String> = model.quality_tags.iter().cloned().collect();
    for alias in aliases {
        match alias.as_str() {
            "frontier" | "tier/frontier" => {
                tags.insert("frontier".to_string());
            }
            "mid" | "tier/mid" => {
                tags.insert("balanced".to_string());
            }
            "small" | "tier/small" => {
                tags.insert("small".to_string());
            }
            _ => {}
        }
    }
    if is_local_provider(&model.provider) {
        tags.insert("local".to_string());
    }
    tags.into_iter().collect()
}

fn aliases_by_model(aliases: &[(String, AliasDef)]) -> BTreeMap<(String, String), Vec<String>> {
    let mut by_model: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (name, alias) in aliases {
        by_model
            .entry((alias.provider.clone(), alias.id.clone()))
            .or_default()
            .push(name.clone());
    }
    for names in by_model.values_mut() {
        names.sort();
    }
    by_model
}

fn modalities_from_caps(caps: &llm::capabilities::Capabilities) -> ModelModalities {
    let mut input = vec!["text".to_string()];
    if caps.vision || caps.vision_supported {
        input.push("image".to_string());
    }
    if caps.audio {
        input.push("audio".to_string());
    }
    if caps.pdf {
        input.push("pdf".to_string());
    }
    if caps.video {
        input.push("video".to_string());
    }
    ModelModalities {
        input,
        output: vec!["text".to_string()],
    }
}

fn catalog_variants(models: &[CatalogModel], aliases: &[CatalogAlias]) -> Vec<CatalogVariant> {
    let mut variants = Vec::new();
    for (id, label, description, alias_name) in [
        (
            "fast",
            "Fast",
            "Lowest-latency general coding-agent route.",
            "small",
        ),
        (
            "balanced",
            "Balanced",
            "Default cost/quality tradeoff for routine coding-agent work.",
            "mid",
        ),
        (
            "high-reasoning",
            "High reasoning",
            "Frontier route for hard planning, repair, and review tasks.",
            "frontier",
        ),
    ] {
        if let Some(alias) = aliases.iter().find(|alias| alias.name == alias_name) {
            variants.push(CatalogVariant {
                id: id.to_string(),
                label: label.to_string(),
                description: description.to_string(),
                model_id: alias.model_id.clone(),
                provider: alias.provider.clone(),
                source: format!("alias:{alias_name}"),
            });
        }
    }
    push_variant_from_model(
        &mut variants,
        "local",
        "Local",
        "Best local/offline model route in the checked-in catalog.",
        models
            .iter()
            .filter(|model| is_local_provider(&model.provider))
            .max_by_key(|model| model.context_window),
    );
    push_variant_from_model(
        &mut variants,
        "cheap",
        "Cheap",
        "Lowest known hosted input+output token price.",
        models
            .iter()
            .filter(|model| !is_local_provider(&model.provider))
            .min_by(|left, right| {
                pricing_total(left)
                    .partial_cmp(&pricing_total(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
    );
    push_variant_from_model(
        &mut variants,
        "vision-capable",
        "Vision capable",
        "A model route that accepts image input.",
        models
            .iter()
            .filter(|model| model.modalities.input.iter().any(|mode| mode == "image"))
            .max_by_key(|model| model.context_window),
    );
    push_variant_from_model(
        &mut variants,
        "long-context",
        "Long context",
        "Largest context-window route in the checked-in catalog.",
        models.iter().max_by_key(|model| model.context_window),
    );
    variants
}

fn push_variant_from_model(
    variants: &mut Vec<CatalogVariant>,
    id: &str,
    label: &str,
    description: &str,
    model: Option<&CatalogModel>,
) {
    if let Some(model) = model {
        variants.push(CatalogVariant {
            id: id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            model_id: model.id.clone(),
            provider: model.provider.clone(),
            source: "catalog".to_string(),
        });
    }
}

fn pricing_total(model: &CatalogModel) -> f64 {
    model
        .pricing
        .as_ref()
        .map(|pricing| pricing.input_per_mtok + pricing.output_per_mtok)
        .unwrap_or(f64::MAX)
}

fn validate_pricing(
    model: &CatalogModel,
    pricing: &ModelPricing,
    result: &mut ProviderCatalogValidation,
) {
    for (field, value) in [
        ("input_per_mtok", Some(pricing.input_per_mtok)),
        ("output_per_mtok", Some(pricing.output_per_mtok)),
        ("cache_read_per_mtok", pricing.cache_read_per_mtok),
        ("cache_write_per_mtok", pricing.cache_write_per_mtok),
    ] {
        if value.is_some_and(|value| value < 0.0) {
            result.errors.push(format!(
                "model {} pricing.{} must be non-negative",
                model.id, field
            ));
        }
    }
}

fn validate_batch_support(
    model: &CatalogModel,
    batch: &ModelBatchSupport,
    result: &mut ProviderCatalogValidation,
) {
    if !matches!(
        batch.wire_format.as_str(),
        "openai" | "anthropic_messages" | "gemini" | "mistral" | "fireworks" | "xai"
    ) {
        result.errors.push(format!(
            "model {} batch.wire_format {:?} is not one of openai|anthropic_messages|gemini|mistral|fireworks|xai",
            model.id, batch.wire_format
        ));
    }
    if !matches!(
        batch.input_mode.as_str(),
        "jsonl_file" | "inline_requests" | "jsonl_or_inline"
    ) {
        result.errors.push(format!(
            "model {} batch.input_mode {:?} is not one of jsonl_file|inline_requests|jsonl_or_inline",
            model.id, batch.input_mode
        ));
    }
    if batch
        .discount_percent
        .is_some_and(|discount| discount > 100)
    {
        result.errors.push(format!(
            "model {} batch.discount_percent must be <= 100",
            model.id
        ));
    }
    for (field, value) in [
        ("turnaround_hours", batch.turnaround_hours.map(u64::from)),
        ("max_requests", batch.max_requests),
        ("max_input_bytes", batch.max_input_bytes),
        (
            "result_retention_days",
            batch.result_retention_days.map(u64::from),
        ),
    ] {
        if value.is_some_and(|value| value == 0) {
            result
                .errors
                .push(format!("model {} batch.{field} must be positive", model.id));
        }
    }
    for note in &batch.security_notes {
        if note.trim().is_empty() {
            result.errors.push(format!(
                "model {} batch.security_notes cannot contain empty notes",
                model.id
            ));
        }
        if note.contains("API_KEY") || note.contains("sk-") {
            result.errors.push(format!(
                "model {} batch.security_notes must not include credential-looking values",
                model.id
            ));
        }
    }
}

fn validate_rate_limits(
    owner: &str,
    rate_limits: &RateLimitsDef,
    result: &mut ProviderCatalogValidation,
) {
    for (field, value) in [
        ("tier", rate_limits.tier.as_deref()),
        ("source_url", rate_limits.source_url.as_deref()),
        ("last_verified", rate_limits.last_verified.as_deref()),
        ("notes", rate_limits.notes.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result
                .errors
                .push(format!("{owner} rate_limits.{field} cannot be empty"));
        }
    }
    if let Some(source_url) = rate_limits.source_url.as_deref() {
        if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
            result.warnings.push(format!(
                "{owner} rate_limits.source_url should be an absolute URL"
            ));
        }
    }
    validate_last_verified(
        owner,
        "rate_limits.last_verified",
        rate_limits.last_verified.as_deref(),
        result,
    );
}

fn validate_performance(
    owner: &str,
    performance: &ServingPerformanceDef,
    result: &mut ProviderCatalogValidation,
) {
    for (field, value) in [
        ("output_tokens_per_sec", performance.output_tokens_per_sec),
        ("time_to_answer_s", performance.time_to_answer_s),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value <= 0.0) {
            result
                .errors
                .push(format!("{owner} performance.{field} must be positive"));
        }
    }
    if performance.sample_size.is_some_and(|value| value == 0) {
        result
            .errors
            .push(format!("{owner} performance.sample_size must be positive"));
    }
    for (field, value) in [
        ("source", performance.source.as_deref()),
        ("source_url", performance.source_url.as_deref()),
        ("last_verified", performance.last_verified.as_deref()),
        ("notes", performance.notes.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result
                .errors
                .push(format!("{owner} performance.{field} cannot be empty"));
        }
    }
    if let Some(source_url) = performance.source_url.as_deref() {
        if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
            result.warnings.push(format!(
                "{owner} performance.source_url should be an absolute URL"
            ));
        }
    }
    validate_last_verified(
        owner,
        "performance.last_verified",
        performance.last_verified.as_deref(),
        result,
    );
}

fn validate_extra_headers(provider: &CatalogProvider, result: &mut ProviderCatalogValidation) {
    for (name, value) in &provider.extra_headers {
        if name.trim().is_empty() {
            result.errors.push(format!(
                "provider {} extra_headers key cannot be empty",
                provider.id
            ));
        }
        if value.trim().is_empty() {
            result.errors.push(format!(
                "provider {} extra_headers.{} cannot be empty",
                provider.id, name
            ));
        }
    }
}

fn validate_provider_healthcheck(
    provider: &CatalogProvider,
    healthcheck: &CatalogProviderHealthcheck,
    result: &mut ProviderCatalogValidation,
) {
    let owner = format!("provider {}", provider.id);
    if healthcheck.method.trim().is_empty() {
        result
            .errors
            .push(format!("{owner} healthcheck.method cannot be empty"));
    }
    if healthcheck.path.is_none() && healthcheck.url.is_none() {
        result.errors.push(format!(
            "{owner} healthcheck must declare either path or url"
        ));
    }
    for (field, value) in [
        ("path", healthcheck.path.as_deref()),
        ("url", healthcheck.url.as_deref()),
        ("body", healthcheck.body.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result
                .errors
                .push(format!("{owner} healthcheck.{field} cannot be empty"));
        }
    }
    if let Some(url) = healthcheck.url.as_deref() {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            result
                .warnings
                .push(format!("{owner} healthcheck.url should be an absolute URL"));
        }
    }
}

fn validate_local_runtime(
    provider_id: &str,
    runtime: &llm_config::LocalRuntimeDef,
    result: &mut ProviderCatalogValidation,
) {
    let owner = format!("provider {provider_id}");
    if let Some(kind) = runtime.kind.as_deref() {
        if !matches!(kind, "daemon_api" | "managed_process" | "external") {
            result.errors.push(format!(
                "{owner} local_runtime.kind must be daemon_api, managed_process, or external"
            ));
        }
    } else {
        result
            .errors
            .push(format!("{owner} local_runtime.kind cannot be empty"));
    }
    if runtime.kind.as_deref() == Some("managed_process")
        && runtime
            .command
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        result
            .errors
            .push(format!("{owner} local_runtime.command cannot be empty"));
    }
    for (field, value) in [
        ("command", runtime.command.as_deref()),
        ("model_source", runtime.model_source.as_deref()),
        ("model_source_env", runtime.model_source_env.as_deref()),
        ("model_arg", runtime.model_arg.as_deref()),
        ("served_model_arg", runtime.served_model_arg.as_deref()),
        ("host_arg", runtime.host_arg.as_deref()),
        ("port_arg", runtime.port_arg.as_deref()),
        ("ctx_arg", runtime.ctx_arg.as_deref()),
        ("parallel_arg", runtime.parallel_arg.as_deref()),
        ("gpu_layers_arg", runtime.gpu_layers_arg.as_deref()),
        ("cache_type_k_arg", runtime.cache_type_k_arg.as_deref()),
        ("cache_type_v_arg", runtime.cache_type_v_arg.as_deref()),
        ("cache_ram_arg", runtime.cache_ram_arg.as_deref()),
        ("enable_lora_arg", runtime.enable_lora_arg.as_deref()),
        ("lora_modules_arg", runtime.lora_modules_arg.as_deref()),
        (
            "lora_modules_value_format",
            runtime.lora_modules_value_format.as_deref(),
        ),
        ("max_lora_rank_arg", runtime.max_lora_rank_arg.as_deref()),
        ("stop", runtime.stop.as_deref()),
        ("source_url", runtime.source_url.as_deref()),
        ("last_verified", runtime.last_verified.as_deref()),
        ("notes", runtime.notes.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result
                .errors
                .push(format!("{owner} local_runtime.{field} cannot be empty"));
        }
    }
    if let Some(stop) = runtime.stop.as_deref() {
        if !matches!(stop, "keep_alive_zero" | "pid" | "external") {
            result.errors.push(format!(
                "{owner} local_runtime.stop must be keep_alive_zero, pid, or external"
            ));
        }
    }
    if let Some(format) = runtime.lora_modules_value_format.as_deref() {
        if !matches!(format, "name_path" | "json_with_base_model") {
            result.errors.push(format!(
                "{owner} local_runtime.lora_modules_value_format must be name_path or json_with_base_model"
            ));
        }
    }
    if let Some(source_url) = runtime.source_url.as_deref() {
        if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
            result.warnings.push(format!(
                "{owner} local_runtime.source_url should be an absolute URL"
            ));
        }
    }
    validate_last_verified(
        &owner,
        "local_runtime.last_verified",
        runtime.last_verified.as_deref(),
        result,
    );
}

fn validate_architecture(
    model: &CatalogModel,
    architecture: &ModelArchitectureDef,
    result: &mut ProviderCatalogValidation,
) {
    if let (Some(active), Some(total)) = (
        architecture.active_parameter_count_b,
        architecture.parameter_count_b,
    ) {
        if active > total {
            result.errors.push(format!(
                "model {} architecture.active_parameter_count_b cannot exceed parameter_count_b",
                model.id
            ));
        }
    }
    for (field, value) in [
        ("quantization", architecture.quantization.as_deref()),
        ("precision", architecture.precision.as_deref()),
        ("license", architecture.license.as_deref()),
        ("tokenizer", architecture.tokenizer.as_deref()),
        ("knowledge_cutoff", architecture.knowledge_cutoff.as_deref()),
        ("source_url", architecture.source_url.as_deref()),
        ("last_verified", architecture.last_verified.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result.errors.push(format!(
                "model {} architecture.{field} cannot be empty",
                model.id
            ));
        }
    }
    if let Some(source_url) = architecture.source_url.as_deref() {
        if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
            result.warnings.push(format!(
                "model {} architecture.source_url should be an absolute URL",
                model.id
            ));
        }
    }
    validate_last_verified(
        &format!("model {}", model.id),
        "architecture.last_verified",
        architecture.last_verified.as_deref(),
        result,
    );
}

fn validate_local_memory(
    model: &CatalogModel,
    memory: &LocalMemoryDef,
    result: &mut ProviderCatalogValidation,
) {
    for (field, value) in [
        ("measured_resident_gib", memory.measured_resident_gib),
        ("base_resident_gib", memory.base_resident_gib),
        ("kv_cache_gib_per_1k_ctx", memory.kv_cache_gib_per_1k_ctx),
        ("safety_margin_gib", memory.safety_margin_gib),
    ] {
        if value.is_some_and(|value| value < 0.0) {
            result.errors.push(format!(
                "model {} local_memory.{field} must be non-negative",
                model.id
            ));
        }
    }
    for (cache_type, multiplier) in &memory.cache_type_multipliers {
        if cache_type.trim().is_empty() {
            result.errors.push(format!(
                "model {} local_memory.cache_type_multipliers cannot contain an empty cache type",
                model.id
            ));
        }
        if *multiplier <= 0.0 {
            result.errors.push(format!(
                "model {} local_memory.cache_type_multipliers.{cache_type} must be positive",
                model.id
            ));
        }
    }
    for (field, value) in [
        ("measured_cache_type", memory.measured_cache_type.as_deref()),
        ("default_cache_type", memory.default_cache_type.as_deref()),
        ("source_url", memory.source_url.as_deref()),
        ("last_verified", memory.last_verified.as_deref()),
        ("notes", memory.notes.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result.errors.push(format!(
                "model {} local_memory.{field} cannot be empty",
                model.id
            ));
        }
    }
    if let Some(source_url) = memory.source_url.as_deref() {
        if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
            result.warnings.push(format!(
                "model {} local_memory.source_url should be an absolute URL",
                model.id
            ));
        }
    }
    if let Some(max_ctx) = memory.max_recommended_context {
        if max_ctx > model.context_window {
            result.warnings.push(format!(
                "model {} local_memory.max_recommended_context exceeds context_window",
                model.id
            ));
        }
    }
    validate_last_verified(
        &format!("model {}", model.id),
        "local_memory.last_verified",
        memory.last_verified.as_deref(),
        result,
    );
}

fn validate_last_verified(
    owner: &str,
    field: &str,
    value: Option<&str>,
    result: &mut ProviderCatalogValidation,
) {
    let Some(value) = value else {
        return;
    };
    let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") else {
        result
            .warnings
            .push(format!("{owner} {field} should use YYYY-MM-DD"));
        return;
    };
    let age_days = Utc::now()
        .date_naive()
        .signed_duration_since(date)
        .num_days();
    if age_days < 0 {
        result
            .warnings
            .push(format!("{owner} {field} is in the future"));
    } else if age_days > FACT_FRESHNESS_WARNING_DAYS {
        result.warnings.push(format!(
            "{owner} {field} is {age_days} days old; refresh provider facts"
        ));
    }
}

fn validate_token_field(
    model: &CatalogModel,
    field: &str,
    value: &str,
    result: &mut ProviderCatalogValidation,
) {
    if !is_catalog_token(value) {
        result.errors.push(format!(
            "model {} {field} must be a lowercase catalog token, got {:?}",
            model.id, value
        ));
    }
}

fn validate_route_token(
    provider: &str,
    model: &str,
    field: &str,
    value: &str,
    result: &mut ProviderCatalogValidation,
) {
    if !is_catalog_token(value) {
        result.errors.push(format!(
            "routing route {provider}:{model} {field} must be a lowercase catalog token, got {value:?}"
        ));
    }
}

fn validate_reviewer_selector(
    model: &CatalogModel,
    value: &str,
    result: &mut ProviderCatalogValidation,
) {
    if value.trim().is_empty() {
        result.errors.push(format!(
            "model {} avoid_as_reviewer_for cannot contain an empty selector",
            model.id
        ));
    }
}

fn is_catalog_token(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn provider_classification(provider: &ProviderDef) -> ProviderClassification {
    if provider.auth_style == "none"
        || provider.base_url.contains("localhost")
        || provider.base_url.contains("127.0.0.1")
    {
        ProviderClassification::Local
    } else {
        ProviderClassification::Hosted
    }
}

fn provider_protocols(id: &str, provider: &ProviderDef) -> Vec<String> {
    match id {
        "anthropic" => vec!["anthropic_messages".to_string()],
        "gemini" => vec!["gemini_generate_content".to_string()],
        "vertex" => vec!["vertex_generate_content".to_string()],
        "bedrock" => vec!["bedrock_converse".to_string()],
        "azure_openai" => vec!["azure_openai_chat_completions".to_string()],
        "ollama" if provider.chat_endpoint.starts_with("/api/") => {
            vec!["ollama_native".to_string()]
        }
        _ => vec!["openai_chat_completions".to_string()],
    }
}

fn provider_caveats(id: &str, provider: &ProviderDef) -> Vec<String> {
    let mut caveats = Vec::new();
    if provider.auth_style == "aws_sigv4" {
        caveats.push("Credentials are resolved through the AWS SDK chain.".to_string());
    }
    if id == "azure_openai" {
        caveats.push("The Harn model field names the Azure deployment.".to_string());
    }
    if id == "ollama" && provider.chat_endpoint == "/api/chat" {
        caveats.push(
            "Native Ollama chat returns NDJSON and can apply model-family parsers.".to_string(),
        );
    }
    caveats
}

fn is_local_provider(provider: &str) -> bool {
    matches!(
        provider,
        "ollama" | "local" | "llamacpp" | "mlx" | "vllm" | "tgi"
    )
}

fn is_tier_alias(name: &str) -> bool {
    matches!(
        name,
        "frontier"
            | "mid"
            | "small"
            | "tier/frontier"
            | "tier/mid"
            | "tier/small"
            | "sonnet"
            | "opus"
            | "haiku"
    )
}

fn title_case(id: &str) -> String {
    id.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
