//! The artifact -> runtime-config direction of the provider catalog.
//!
//! The rest of `provider_catalog` compiles the loaded configuration *into* a
//! versioned artifact. These functions run it backwards: a signed remote
//! catalog arrives as an artifact and has to become the `ProvidersConfig` the
//! runtime already knows how to resolve. Keeping the inverse in its own file
//! makes the round trip legible — every field that survives the trip out has
//! a matching line here, and a field that silently does not is easy to spot.

use super::*;

pub(crate) fn config_from_artifact(
    artifact: &ProviderCatalogArtifact,
) -> llm_config::ProvidersConfig {
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
        presentation: llm_config::PresentationConfig {
            // A remote artifact already resolved every recommendation. Preserve
            // that exact choice as a fixed model selector when installing the
            // runtime overlay instead of re-running a dynamic selector against
            // a potentially different local catalog.
            variants: artifact
                .variants
                .iter()
                .enumerate()
                .map(|(index, variant)| {
                    (
                        variant.id.clone(),
                        llm_config::PresentationVariantDef {
                            order: u16::try_from(index).unwrap_or(u16::MAX),
                            label: variant.label.clone(),
                            description: variant.description.clone(),
                            selector: llm_config::PresentationVariantSelector::Model {
                                model_id: variant.model_id.clone(),
                            },
                            automatic_eligibility: variant.automatic_eligibility.clone(),
                        },
                    )
                })
                .collect(),
            families: artifact
                .families
                .iter()
                .map(|family| {
                    (
                        family.id.clone(),
                        llm_config::ModelFamilyDef {
                            label: family.label.clone(),
                            plain_description: family.plain_description.clone(),
                            model_id: family.model_id.clone(),
                            dimensions: family.dimensions.clone(),
                            presets: family.presets.clone(),
                        },
                    )
                })
                .collect(),
            // A remote artifact carries no curated short list, and an empty
            // list means "keep the local one" at merge time. Serving a remote
            // catalog must not silently retire the local setup guidance.
            featured_providers: Vec::new(),
        },
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
        embeddings_endpoint: provider.endpoint.embeddings_endpoint.clone(),
        healthcheck: provider
            .healthcheck
            .clone()
            .map(healthcheck_def_from_catalog),
        cache_usage_accounting: provider.cache_usage_accounting,
        data_controls: provider
            .data_controls
            .as_ref()
            .map(CatalogProviderDataControls::to_definition),
        stream_usage_accounting: provider.stream_usage_accounting,
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
        display_name: Some(model.display_name.clone()),
        blurb: model.blurb.clone(),
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
        data_controls: model.data_controls.clone(),
        deprecated: model.deprecation.status == DeprecationStatus::Deprecated,
        deprecation_note: model.deprecation.note.clone(),
        sunset_date: model.deprecation.sunset_date.clone(),
        superseded_by: model.deprecation.superseded_by.clone(),
        serving_tiers: model.serving_tiers.clone(),
        reasoning_modes: model.reasoning_modes.clone(),
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
        completion_review: model.completion_review.clone(),
        released: model.released.clone(),
        row_kind: model.row_kind,
        current_snapshot: model.current_snapshot.clone(),
        embedding_dim: model.embedding_dim,
        embedding_max_tokens: model.embedding_max_tokens,
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
