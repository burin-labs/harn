//! Selector resolution: turn an alias or provider/model selector into the
//! complete `ResolvedModel` identity (provider, normalized id, tool format,
//! tier, family, lineage).
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedModel {
    pub id: String,
    pub provider: String,
    pub alias: Option<String>,
    pub tool_format: String,
    pub tier: String,
    pub family: String,
    pub lineage: String,
}

/// Version of the compiled model catalog used for a resolution decision.
///
/// The catalog ships as part of `harn-vm`, so the crate version is the stable
/// public identity that lets a diagnostic be reproduced against the same
/// rows. Ambient overlays do not masquerade as a different shipped catalog;
/// their resolved entries are still captured in the resolution receipt.
pub const MODEL_CATALOG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One total model-selection decision, before any provider transport starts.
///
/// This is the semantic owner for the route facts persisted in run
/// receipts. Hosts should carry this record instead of independently resolving
/// the provider and model from the same selector.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelResolution {
    pub requested_model: String,
    pub alias_chain: Vec<String>,
    pub resolved_provider: String,
    pub resolved_model: String,
    pub catalog_version: String,
}

impl ModelResolution {
    /// Snap a decision to a later routing-policy result without losing the
    /// original selector or alias chain. A provider-qualified original remains
    /// a hard constraint, including across automatic routing layers.
    pub fn resolve_route(
        &mut self,
        provider: &str,
        model: &str,
    ) -> Result<(), ModelResolutionError> {
        let config = effective_config();
        let route = resolve_model_request_with_config(
            &config,
            model,
            Some(provider),
            ProviderResolutionScope::ActiveCall,
        )?;
        if let Some((requested_provider, _)) = self.requested_model.split_once(':') {
            if let Some(requested_provider) = known_provider(
                &config,
                provider_selector_target(requested_provider),
                ProviderResolutionScope::Catalog,
            ) {
                if requested_provider != route.resolved_provider {
                    return Err(ModelResolutionError::ProviderConflict {
                        selector_provider: requested_provider.to_string(),
                        requested_provider: route.resolved_provider,
                        catalog_version: catalog_version(),
                    });
                }
            }
        }
        self.resolved_provider = route.resolved_provider;
        self.resolved_model = route.resolved_model;
        Ok(())
    }

    pub fn assert_resolved_route(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<(), ModelResolutionError> {
        if self.resolved_provider == provider && self.resolved_model == model {
            return Ok(());
        }
        Err(ModelResolutionError::ResolvedRouteMismatch {
            receipt_provider: self.resolved_provider.clone(),
            receipt_model: self.resolved_model.clone(),
            transport_provider: provider.to_string(),
            transport_model: model.to_string(),
            catalog_version: self.catalog_version.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResolutionError {
    EmptyModel {
        catalog_version: String,
    },
    UnknownProvider {
        provider: String,
        catalog_version: String,
        suggestions: Vec<String>,
    },
    ProviderConflict {
        selector_provider: String,
        requested_provider: String,
        catalog_version: String,
    },
    ProviderModelMismatch {
        provider: String,
        model: String,
        catalog_provider: String,
        catalog_version: String,
    },
    UnknownModel {
        model: String,
        catalog_version: String,
        suggestions: Vec<String>,
    },
    AliasCycle {
        alias_chain: Vec<String>,
        catalog_version: String,
    },
    ResolvedRouteMismatch {
        receipt_provider: String,
        receipt_model: String,
        transport_provider: String,
        transport_model: String,
        catalog_version: String,
    },
}

impl ModelResolutionError {
    pub fn catalog_version(&self) -> &str {
        match self {
            Self::EmptyModel { catalog_version }
            | Self::UnknownProvider {
                catalog_version, ..
            }
            | Self::ProviderConflict {
                catalog_version, ..
            }
            | Self::ProviderModelMismatch {
                catalog_version, ..
            }
            | Self::UnknownModel {
                catalog_version, ..
            }
            | Self::AliasCycle {
                catalog_version, ..
            }
            | Self::ResolvedRouteMismatch {
                catalog_version, ..
            } => catalog_version,
        }
    }
}

impl std::fmt::Display for ModelResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let suggestions = |items: &[String]| {
            if items.is_empty() {
                String::new()
            } else {
                format!("; did you mean {}?", items.join(", "))
            }
        };
        match self {
            Self::EmptyModel { catalog_version } => write!(
                f,
                "model selector is empty (catalog {catalog_version})"
            ),
            Self::UnknownProvider {
                provider,
                catalog_version,
                suggestions: near,
            } => write!(
                f,
                "unknown model provider '{provider}' in catalog {catalog_version}{}",
                suggestions(near)
            ),
            Self::ProviderConflict {
                selector_provider,
                requested_provider,
                catalog_version,
            } => write!(
                f,
                "model selector requests provider '{selector_provider}' but the call requests provider '{requested_provider}' (catalog {catalog_version})"
            ),
            Self::ProviderModelMismatch {
                provider,
                model,
                catalog_provider,
                catalog_version,
            } => write!(
                f,
                "model '{model}' is catalogued for provider '{catalog_provider}', not requested provider '{provider}' (catalog {catalog_version})"
            ),
            Self::UnknownModel {
                model,
                catalog_version,
                suggestions: near,
            } => write!(
                f,
                "unknown model or alias '{model}' in catalog {catalog_version}{}",
                suggestions(near)
            ),
            Self::AliasCycle {
                alias_chain,
                catalog_version,
            } => write!(
                f,
                "model alias cycle in catalog {catalog_version}: {}",
                alias_chain.join(" -> ")
            ),
            Self::ResolvedRouteMismatch {
                receipt_provider,
                receipt_model,
                transport_provider,
                transport_model,
                catalog_version,
            } => write!(
                f,
                "resolved model receipt '{receipt_provider}:{receipt_model}' does not match transport route '{transport_provider}:{transport_model}' (catalog {catalog_version})"
            ),
        }
    }
}

impl std::error::Error for ModelResolutionError {}

fn catalog_version() -> String {
    MODEL_CATALOG_VERSION.to_string()
}

fn nearest_names<'a>(requested: &str, candidates: impl Iterator<Item = &'a str>) -> Vec<String> {
    let requested = requested.to_ascii_lowercase();
    let maximum_distance = (requested.chars().count() / 4).clamp(2, 4);
    let mut ranked: Vec<_> = candidates
        .map(|candidate| {
            (
                strsim::levenshtein(&requested, &candidate.to_ascii_lowercase()),
                candidate,
            )
        })
        .filter(|(distance, _)| *distance <= maximum_distance)
        .collect();
    ranked.sort_by(|(distance_a, name_a), (distance_b, name_b)| {
        distance_a.cmp(distance_b).then_with(|| name_a.cmp(name_b))
    });
    ranked.dedup_by(|(_, name_a), (_, name_b)| name_a == name_b);
    ranked
        .into_iter()
        .take(3)
        .map(|(_, name)| name.to_string())
        .collect()
}

fn provider_selector_target(provider: &str) -> &str {
    match provider {
        "local" => "ollama",
        "hf" => "huggingface",
        other => other,
    }
}

fn explicit_provider_target(provider: &str) -> &str {
    match provider {
        // `local` is both a real generic OpenAI-compatible adapter and a
        // selector shorthand for Ollama. Only selector syntax takes the
        // shorthand; an explicit provider names the adapter itself.
        "hf" => "huggingface",
        other => other,
    }
}

#[derive(Clone, Copy)]
enum ProviderResolutionScope {
    Catalog,
    ActiveCall,
}

impl ProviderResolutionScope {
    fn permits_fixture_adapter(self) -> bool {
        matches!(self, Self::ActiveCall)
            && (crate::llm::mock::cli_llm_mock_replay_active()
                || crate::llm::mock::builtin_llm_mock_active())
    }
}

fn known_provider<'a>(
    config: &ProvidersConfig,
    provider: &'a str,
    scope: ProviderResolutionScope,
) -> Option<&'a str> {
    let provider = if config.providers.contains_key(provider)
        || matches!(provider, "mock" | "fake")
        || crate::llm::provider::is_provider_registered(provider)
    {
        provider
    } else {
        provider_selector_target(provider)
    };
    (matches!(provider, "mock" | "fake")
        || config.providers.contains_key(provider)
        || crate::llm::provider::is_provider_registered(provider)
        // A scoped fixture is itself the transport adapter. Its scripted
        // provider/model identity is intentionally open-world so tests can
        // prove routing and receipts without registering a fake production
        // endpoint. Catalog introspection and real calls still fail closed.
        || scope.permits_fixture_adapter())
    .then_some(provider)
}

const MODEL_PROXY_FEATURE: &str = "model_proxy";

fn provider_has_builtin_model_namespace(config: &ProvidersConfig, provider: &str) -> bool {
    static PROVIDERS: OnceLock<BTreeSet<String>> = OnceLock::new();
    let is_builtin = PROVIDERS
        .get_or_init(|| default_config().providers.into_keys().collect())
        .contains(provider);
    let is_proxy = config.providers.get(provider).is_some_and(|definition| {
        definition
            .features
            .iter()
            .any(|feature| feature == MODEL_PROXY_FEATURE)
    });
    is_builtin && !is_proxy
}

fn provider_suggestions(config: &ProvidersConfig, requested: &str) -> Vec<String> {
    nearest_names(
        requested,
        config
            .providers
            .keys()
            .map(String::as_str)
            .chain(["local", "hf", "mock"]),
    )
}

fn model_suggestions(config: &ProvidersConfig, requested: &str) -> Vec<String> {
    nearest_names(
        requested,
        config
            .aliases
            .keys()
            .map(String::as_str)
            .chain(config.models.keys().map(String::as_str)),
    )
}

/// Resolve a caller's model and optional provider constraint as one decision.
///
/// A provider-qualified selector is a hard constraint. A catalog row owned by
/// another provider therefore fails before credentials or transport are
/// touched. Unknown provider-native ids remain valid when they are explicitly
/// constrained to a known provider, preserving private deployments and newly
/// released models; near-miss aliases fail with catalog-versioned suggestions.
pub fn resolve_model_request(
    requested_model: &str,
    requested_provider: Option<&str>,
) -> Result<ModelResolution, ModelResolutionError> {
    let config = effective_config();
    resolve_model_request_with_config(
        &config,
        requested_model,
        requested_provider,
        ProviderResolutionScope::Catalog,
    )
}

pub(crate) fn resolve_model_request_for_active_call(
    requested_model: &str,
    requested_provider: Option<&str>,
) -> Result<ModelResolution, ModelResolutionError> {
    let config = effective_config();
    resolve_model_request_with_config(
        &config,
        requested_model,
        requested_provider,
        ProviderResolutionScope::ActiveCall,
    )
}

fn resolve_model_request_with_config(
    config: &ProvidersConfig,
    requested_model: &str,
    requested_provider: Option<&str>,
    scope: ProviderResolutionScope,
) -> Result<ModelResolution, ModelResolutionError> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return Err(ModelResolutionError::EmptyModel {
            catalog_version: catalog_version(),
        });
    }

    let explicit_provider = requested_provider
        .map(str::trim)
        .filter(|provider| !provider.is_empty() && !provider.eq_ignore_ascii_case("auto"))
        .map(|provider| {
            known_provider(config, explicit_provider_target(provider), scope).ok_or_else(|| {
                ModelResolutionError::UnknownProvider {
                    provider: provider.to_string(),
                    catalog_version: catalog_version(),
                    suggestions: provider_suggestions(config, provider),
                }
            })
        })
        .transpose()?;

    let qualified_parts = requested_model
        .split_once(':')
        .filter(|(_, model)| !model.trim().is_empty());
    let qualified = qualified_parts.and_then(|(provider, model)| {
        // Selector prefixes are syntax, so only registered or configured
        // names may claim them. Fixture adapters remain open-world through
        // the separate explicit `provider` field; otherwise a provider-native
        // model id such as `qwen3.2:latest` becomes an accidental qualifier.
        known_provider(
            config,
            provider_selector_target(provider),
            ProviderResolutionScope::Catalog,
        )
        .map(|known| (known, model.trim()))
    });
    if let Some((provider, model)) = qualified_parts.filter(|_| qualified.is_none()) {
        let suggestions = provider_suggestions(config, provider);
        let suffix_is_catalogued =
            config.aliases.contains_key(model) || config.models.contains_key(model);
        let unqualified_prefix_looks_misspelled =
            explicit_provider.is_none() && !suggestions.is_empty();
        if suffix_is_catalogued || unqualified_prefix_looks_misspelled {
            return Err(ModelResolutionError::UnknownProvider {
                provider: provider.to_string(),
                catalog_version: catalog_version(),
                suggestions,
            });
        }
    }
    if let (Some(explicit), Some((qualified_provider, _))) = (explicit_provider, qualified) {
        if explicit != qualified_provider {
            return Err(ModelResolutionError::ProviderConflict {
                selector_provider: qualified_provider.to_string(),
                requested_provider: explicit.to_string(),
                catalog_version: catalog_version(),
            });
        }
    }

    let provider_constraint = qualified
        .map(|(provider, _)| provider)
        .or(explicit_provider);
    let mut current = qualified
        .map(|(_, model)| model)
        .unwrap_or(requested_model)
        .to_string();
    let mut alias_chain = Vec::new();
    let mut alias_provider = None;
    let mut visited = std::collections::BTreeSet::new();
    while let Some(alias) = config.aliases.get(&current) {
        if !visited.insert(current.clone()) {
            alias_chain.push(current);
            return Err(ModelResolutionError::AliasCycle {
                alias_chain,
                catalog_version: catalog_version(),
            });
        }
        alias_provider = Some(alias.provider.as_str());
        // Some catalog rows use a same-name alias to attach a provider to a
        // provider-native model id. That is a terminal annotation, not a
        // recursive rename and therefore not an alias cycle.
        if alias.id == current {
            break;
        }
        alias_chain.push(current);
        current = alias.id.clone();
    }

    let catalog_row = config.models.get(&current);
    let catalog_provider = catalog_row.map(|row| row.provider.as_str());
    let inferred = infer_provider_with_config(config, &current);
    let resolved_provider = provider_constraint
        .or(alias_provider)
        .or(catalog_provider)
        .unwrap_or(inferred.provider.as_str());

    let inferred_provider = (inferred.source
        == crate::llm::provider::ProviderInferenceSource::BuiltinRule)
        .then_some(inferred.provider.as_str());
    let provider_expectation = provider_constraint
        .map(|expected| {
            (
                expected,
                alias_provider.or(catalog_provider).or(inferred_provider),
            )
        })
        .or_else(|| {
            alias_provider.map(|expected| (expected, catalog_provider.or(inferred_provider)))
        });
    // Catalog-owned providers promise a concrete model namespace, so crossing
    // those namespaces is an error. Runtime-registered adapters remain
    // open-world: test providers and customer proxies commonly serve a model
    // whose canonical identity is catalogued under its upstream provider.
    let enforces_catalog_ownership = provider_constraint
        .or(alias_provider)
        .is_some_and(|provider| provider_has_builtin_model_namespace(config, provider));
    if let Some((expected, Some(actual))) =
        provider_expectation.filter(|_| enforces_catalog_ownership)
    {
        if provider_selector_target(actual) != expected {
            return Err(ModelResolutionError::ProviderModelMismatch {
                provider: expected.to_string(),
                model: current,
                catalog_provider: provider_selector_target(actual).to_string(),
                catalog_version: catalog_version(),
            });
        }
    }

    if provider_constraint.is_none() && alias_chain.is_empty() && catalog_row.is_none() {
        let suggestions = model_suggestions(config, &current);
        if !suggestions.is_empty() {
            return Err(ModelResolutionError::UnknownModel {
                model: current,
                catalog_version: catalog_version(),
                suggestions,
            });
        }
    }

    Ok(ModelResolution {
        requested_model: requested_model.to_string(),
        alias_chain,
        resolved_provider: resolved_provider.to_string(),
        resolved_model: current,
        catalog_version: catalog_version(),
    })
}

/// Stable, secret-free model-route facts suitable for durable receipts.
///
/// The execution path may carry arbitrary route-overlay parameters. This
/// contract exposes only Harn's validated generation-default schema so
/// replay, eval, and audit consumers do not serialize private operator fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelExecutionContract {
    pub resolution: ModelResolution,
    pub tool_format: String,
    pub tier: String,
    pub family: String,
    pub lineage: String,
    pub wire_model: String,
    pub generation_defaults: BTreeMap<String, toml::Value>,
}

/// Resolve a model alias to (model_id, provider_name).
pub fn resolve_model(alias: &str) -> (String, Option<String>) {
    let config = effective_config();
    if let Some(a) = config.aliases.get(alias) {
        return (a.id.clone(), Some(a.provider.clone()));
    }
    (normalize_model_id_with_config(alias, &config), None)
}

/// Strip host/provider selector prefixes that identify transport, not the
/// provider-native model id. This mirrors the host's existing normalization so
/// `ollama:qwen3:30b` and `ollama/qwen3:30b` reach Ollama as
/// `qwen3:30b` instead of an invalid model named `ollama`. Cerebras follows
/// the slash convention (`cerebras/gpt-oss-120b`) because its own /v1/models
/// endpoint returns bare names that overlap OpenAI's families.
pub fn normalize_model_id(raw: &str) -> String {
    normalize_model_id_with_config(raw, &effective_config())
}

fn normalize_model_id_with_config(raw: &str, config: &ProvidersConfig) -> String {
    for prefix in PROVIDER_SELECTOR_PREFIXES {
        if let Some(stripped) = raw.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    if let Some((provider, model)) = raw.split_once(':') {
        if !model.is_empty()
            && (provider == "mock" || configured_provider_selector(config, raw).is_some())
        {
            return model.to_string();
        }
    }
    raw.to_string()
}

const PROVIDER_SELECTOR_PREFIXES: &[&str] = &[
    "ollama:",
    "ollama/",
    "local:",
    "huggingface:",
    "hf:",
    "cerebras/",
];

/// Resolve an alias or selector into the complete catalog identity hosts need:
/// provider inference, prefix-normalized model id, default tool format, and tier.
pub fn resolve_model_info(selector: &str) -> ResolvedModel {
    let config = effective_config();
    if let Some(alias) = config.aliases.get(selector) {
        let id = alias.id.clone();
        let provider = alias.provider.clone();
        let requested = alias
            .tool_format
            .clone()
            .unwrap_or_else(|| default_tool_format_with_config(&config, &id, &provider));
        let tool_format = guard_tool_format(&provider, &id, &requested, Some(selector));
        return ResolvedModel {
            tier: model_tier_with_config(&config, &id),
            family: model_family_with_config(&config, &provider, &id),
            lineage: model_lineage_with_config(&config, &provider, &id),
            id,
            provider,
            alias: Some(selector.to_string()),
            tool_format,
        };
    }

    let id = normalize_model_id_with_config(selector, &config);
    let inference = infer_provider_with_config(&config, selector);
    let source = inference.source;
    let provider = inference.provider;
    let requested = default_tool_format_with_config(&config, &id, &provider);
    let tool_format = guard_tool_format(&provider, &id, &requested, None);
    let tier = model_tier_with_config(&config, &id);
    let family = model_family_with_inference_source(&config, &provider, &id, source);
    let lineage = model_lineage_with_inference_source(&config, &provider, &id, source);
    ResolvedModel {
        id,
        provider,
        alias: None,
        tool_format,
        tier,
        family,
        lineage,
    }
}

/// Resolve a model selector into the stable, secret-free execution facts that
/// hosts may persist and fingerprint.
pub fn model_execution_contract(
    selector: &str,
) -> Result<ModelExecutionContract, ModelResolutionError> {
    let resolution = resolve_model_request(selector, None)?;
    let provider = resolution.resolved_provider.as_str();
    let model = resolution.resolved_model.as_str();
    let requested_tool_format = default_tool_format(model, provider);
    let tool_format = guard_tool_format(
        provider,
        model,
        &requested_tool_format,
        resolution.alias_chain.first().map(String::as_str),
    );
    let wire_model = wire_model_id(model);
    let generation_defaults = generation_defaults_for_route(provider, model);
    Ok(ModelExecutionContract {
        tier: model_tier(model),
        family: model_family(provider, model),
        lineage: model_lineage(provider, model),
        resolution,
        tool_format,
        wire_model,
        generation_defaults,
    })
}

/// Run the requested `tool_format` through the capability registry's
/// dialect-validity gate, returning the safe format to actually use. When the
/// registry auto-corrects a known-broken combo (e.g. a `native` pin on a
/// `native_unreliable` route that silently drops to unparsed DSML text), the
/// correction is logged once at resolution time so a harness developer sees
/// *why* their pinned format was not honored — never a silent vanishing.
fn guard_tool_format(provider: &str, model: &str, requested: &str, alias: Option<&str>) -> String {
    let decision = crate::llm::capabilities::validate_tool_format(provider, model, requested);
    if let Some(reason) = &decision.correction {
        tracing::warn!(
            target: "harn::llm::tool_format",
            alias = alias.unwrap_or(""),
            "{reason}"
        );
    }
    decision.effective
}

#[cfg(test)]
mod tests;
