use std::collections::{BTreeMap, BTreeSet};

use super::*;

pub fn validate_artifact(artifact: &ProviderCatalogArtifact) -> ProviderCatalogValidation {
    let mut result = ProviderCatalogValidation::default();
    if artifact.schema_version != PROVIDER_CATALOG_SCHEMA_VERSION {
        result.errors.push(format!(
            "schema_version must be {}, got {}",
            PROVIDER_CATALOG_SCHEMA_VERSION, artifact.schema_version
        ));
    }
    if artifact.providers.is_empty() {
        result.errors.push("catalog has no providers".to_string());
    }
    if artifact.models.is_empty() {
        result.errors.push("catalog has no models".to_string());
    }

    let provider_ids: BTreeSet<_> = artifact.providers.iter().map(|p| p.id.as_str()).collect();
    for provider in &artifact.providers {
        if provider.id.trim().is_empty() {
            result
                .errors
                .push("provider id cannot be empty".to_string());
        }
        if provider.display_name.trim().is_empty() {
            result.errors.push(format!(
                "provider {} display_name cannot be empty",
                provider.id
            ));
        }
        if provider.endpoint.chat_endpoint.trim().is_empty() {
            result.errors.push(format!(
                "provider {} chat_endpoint cannot be empty",
                provider.id
            ));
        }
        if provider.auth.required
            && provider.auth.env.is_empty()
            && provider.auth.style != "aws_sigv4"
        {
            result.errors.push(format!(
                "provider {} requires auth but declares no auth env keys",
                provider.id
            ));
        }
        if let Some(rate_limits) = &provider.rate_limits {
            validate_rate_limits(
                &format!("provider {}", provider.id),
                rate_limits,
                &mut result,
            );
        }
        if let Some(local_runtime) = &provider.local_runtime {
            validate_local_runtime(&provider.id, local_runtime, &mut result);
        }
    }

    let mut alias_names = BTreeSet::new();
    for alias in &artifact.aliases {
        if alias.name.trim().is_empty() {
            result.errors.push("alias name cannot be empty".to_string());
        }
        if !alias_names.insert(alias.name.as_str()) {
            result
                .errors
                .push(format!("duplicate alias name {}", alias.name));
        }
        if !provider_ids.contains(alias.provider.as_str()) {
            result.errors.push(format!(
                "alias {} references unknown provider {}",
                alias.name, alias.provider
            ));
        }
    }

    let mut model_ids = BTreeSet::new();
    let mut model_pairs = BTreeSet::new();
    for model in &artifact.models {
        if !model_ids.insert(model.id.as_str()) {
            result
                .errors
                .push(format!("duplicate model id {}", model.id));
        }
        model_pairs.insert((model.provider.as_str(), model.id.as_str()));
        if model.name.trim().is_empty() {
            result
                .errors
                .push(format!("model {} name cannot be empty", model.id));
        }
        if !provider_ids.contains(model.provider.as_str()) {
            result.errors.push(format!(
                "model {} references unknown provider {}",
                model.id, model.provider
            ));
        }
        validate_token_field(model, "family", &model.family, &mut result);
        validate_token_field(model, "lineage", &model.lineage, &mut result);
        for family in &model.complementary_with {
            validate_token_field(model, "complementary_with", family, &mut result);
        }
        for selector in &model.avoid_as_reviewer_for {
            validate_reviewer_selector(model, selector, &mut result);
        }
        if model.context_window == 0 {
            result.errors.push(format!(
                "model {} context_window must be positive",
                model.id
            ));
        }
        if let Some(pricing) = &model.pricing {
            validate_pricing(model, pricing, &mut result);
        }
        if let Some(rate_limits) = &model.rate_limits {
            validate_rate_limits(&format!("model {}", model.id), rate_limits, &mut result);
        }
        if let Some(architecture) = &model.architecture {
            validate_architecture(model, architecture, &mut result);
        }
        if let Some(memory) = &model.local_memory {
            validate_local_memory(model, memory, &mut result);
        }
        if model.deprecation.status == DeprecationStatus::Deprecated
            && model
                .deprecation
                .note
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            result.errors.push(format!(
                "deprecated model {} must include deprecation.note",
                model.id
            ));
        }
        if let Some(fast) = &model.fast_mode {
            if let Some(pricing) = &fast.pricing {
                validate_pricing(model, pricing, &mut result);
            }
            if let Some(status) = fast.status.as_deref() {
                if !matches!(status, "ga" | "research_preview" | "deprecated") {
                    result.warnings.push(format!(
                        "model {} fast_mode.status {:?} is not one of ga|research_preview|deprecated",
                        model.id, status
                    ));
                }
            }
        }
    }

    // Structured supersession pointers must reference a real catalog row so
    // `superseded_by` can be trusted as a migration target by downstream
    // tooling. A dangling pointer is a soft warning (the row is still
    // usable) rather than a hard error, mirroring how `note` is advisory.
    for model in &artifact.models {
        if let Some(target) = model.deprecation.superseded_by.as_deref() {
            if !model_ids.contains(target) {
                result.warnings.push(format!(
                    "model {} declares superseded_by {} with no matching catalog row",
                    model.id, target
                ));
            }
        }
    }

    // Index models by (provider, id) so alias tool_format can be checked
    // against the target model's declared tool support. An alias is the one
    // place a harness author can pin `native` / `text` per model, so a typo
    // or a format the model can't serve must be caught at catalog-build time
    // rather than silently degrading at call time.
    let model_by_pair: BTreeMap<(&str, &str), &CatalogModel> = artifact
        .models
        .iter()
        .map(|model| ((model.provider.as_str(), model.id.as_str()), model))
        .collect();

    let dedicated_pairs: BTreeSet<(&str, &str)> = artifact
        .models
        .iter()
        .filter(|model| model.availability == ModelAvailabilityStatus::Dedicated)
        .map(|model| (model.provider.as_str(), model.id.as_str()))
        .collect();
    for alias in &artifact.aliases {
        if !model_pairs.contains(&(alias.provider.as_str(), alias.model_id.as_str())) {
            result.errors.push(format!(
                "alias {} targets {}/{} without a catalog row",
                alias.name, alias.provider, alias.model_id
            ));
        }
        if let Some(format) = alias.tool_format.as_deref() {
            // `json` (fenced-JSON) and `text` (tagged/heredoc) are both
            // TEXT-channel formats and validate against `tool_support.text`;
            // `native` validates against `tool_support.native`.
            if format != "native" && format != "text" && format != "json" {
                result.errors.push(format!(
                    "alias {} declares tool_format {:?}; must be \"native\", \"text\", or \"json\"",
                    alias.name, format
                ));
            } else if let Some(model) =
                model_by_pair.get(&(alias.provider.as_str(), alias.model_id.as_str()))
            {
                if format == "native" && !model.tool_support.native {
                    result.errors.push(format!(
                        "alias {} pins tool_format \"native\" but model {}/{} does not support native tool calling",
                        alias.name, alias.provider, alias.model_id
                    ));
                }
                if (format == "text" || format == "json") && !model.tool_support.text {
                    result.errors.push(format!(
                        "alias {} pins tool_format {:?} (a text-channel format) but model {}/{} does not support text tool calling",
                        alias.name, format, alias.provider, alias.model_id
                    ));
                }
            }
        }
        if is_tier_alias(&alias.name)
            && dedicated_pairs.contains(&(alias.provider.as_str(), alias.model_id.as_str()))
        {
            result.warnings.push(format!(
                "tier alias {} targets dedicated-only model {}/{}; serverless callers will fail until the dedicated endpoint is provisioned",
                alias.name, alias.provider, alias.model_id
            ));
        }
    }

    for variant in &artifact.variants {
        if variant.id.trim().is_empty() {
            result.errors.push("variant id cannot be empty".to_string());
        }
        if !provider_ids.contains(variant.provider.as_str()) {
            result.errors.push(format!(
                "variant {} references unknown provider {}",
                variant.id, variant.provider
            ));
        }
        if !model_pairs.contains(&(variant.provider.as_str(), variant.model_id.as_str())) {
            result.errors.push(format!(
                "variant {} targets {}/{} without a catalog row",
                variant.id, variant.provider, variant.model_id
            ));
        }
    }

    result
}

pub fn validate_current() -> ProviderCatalogValidation {
    validate_artifact(&artifact())
}
