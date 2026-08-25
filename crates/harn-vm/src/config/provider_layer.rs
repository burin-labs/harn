//! Projection from the provider catalog into the canonical config layer.

use serde_json::{json, Map as JsonMap, Value as JsonValue};

use super::{ConfigLayer, ConfigLayerKind};

pub fn layer_from_providers_config(
    kind: ConfigLayerKind,
    name: impl Into<String>,
    source: impl Into<String>,
    providers: &crate::llm_config::ProvidersConfig,
) -> ConfigLayer {
    let mut canonical_providers = JsonMap::new();
    for (provider_name, provider) in &providers.providers {
        canonical_providers.insert(
            provider_name.clone(),
            json!({
                "base_url": provider.base_url,
                "auth_env": crate::llm_config::auth_env_names(&provider.auth_env),
                "capability_refs": provider.features,
                "models": [],
                "metadata": {
                    "auth_style": provider.auth_style,
                    "chat_endpoint": provider.chat_endpoint,
                    "completion_endpoint": provider.completion_endpoint,
                    "embeddings_endpoint": provider.embeddings_endpoint,
                }
            }),
        );
    }
    for (model_id, model) in &providers.models {
        let entry = canonical_providers
            .entry(model.provider.clone())
            .or_insert_with(|| {
                json!({
                    "base_url": null,
                    "auth_env": [],
                    "capability_refs": [],
                    "models": [],
                    "metadata": {}
                })
            });
        if let Some(models) = entry.get_mut("models").and_then(JsonValue::as_array_mut) {
            models.push(JsonValue::String(model_id.clone()));
        }
    }
    let aliases = providers
        .aliases
        .iter()
        .map(|(alias, entry)| {
            (
                alias.clone(),
                json!({
                    "model": entry.id,
                    "provider": entry.provider,
                    "capability_refs": [],
                }),
            )
        })
        .collect::<JsonMap<String, JsonValue>>();
    ConfigLayer::new(
        kind,
        name,
        source,
        json!({
            "models": {
                "default_provider": providers.default_provider,
                "providers": canonical_providers,
                "aliases": aliases,
            }
        }),
    )
}
