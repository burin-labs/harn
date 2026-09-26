use harn_vm::llm_config;

use crate::net;

use super::dto::{PortalLlmOptions, PortalLlmProviderOption};

pub(super) async fn build_llm_options() -> PortalLlmOptions {
    let config = llm_config::load_config();
    let preferred_provider = std::env::var("HARN_LLM_PROVIDER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if std::env::var("MLX_BASE_URL").is_ok() || std::env::var("MLX_MODEL_ID").is_ok() {
                Some("mlx".to_string())
            } else if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
                Some("local".to_string())
            } else {
                None
            }
        });
    let preferred_model = std::env::var("HARN_LLM_MODEL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("MLX_MODEL_ID")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
                std::env::var("LOCAL_LLM_MODEL")
                    .ok()
                    .filter(|value| !value.is_empty())
            } else {
                None
            }
        });

    let mut providers = Vec::new();
    let catalog_models = llm_config::model_catalog_entries();
    for name in llm_config::provider_names() {
        let Some(def) = llm_config::provider_config(&name) else {
            continue;
        };
        let base_url = llm_config::resolve_base_url(&def);
        let auth_envs = llm_config::auth_env_names(&def.auth_env);
        let auth_configured = harn_vm::llm::provider_auth_status(&name).available;
        let local = is_local_provider(&name, &base_url);
        let aliases = config
            .aliases
            .iter()
            .filter(|(_, alias)| alias.provider == name)
            .map(|(alias_name, _)| alias_name.clone())
            .collect::<Vec<_>>();
        let discovered = if local {
            Some(discover_provider_models(&name, &base_url, &def).await)
        } else {
            None
        };
        let viable = auth_configured
            && discovered
                .as_ref()
                .is_none_or(|result| result.as_ref().is_ok_and(|models| !models.is_empty()));
        let mut models = discovered.and_then(Result::ok).unwrap_or_default();
        if !local {
            models.extend(
                catalog_models
                    .iter()
                    .filter(|(_, model)| model.provider == name && !model.deprecated)
                    .map(|(id, model)| model.wire_model.clone().unwrap_or_else(|| id.clone())),
            );
        }
        let default_model = llm_config::portal_default_model_for_provider(&name);
        if let Some(default_model) = &default_model {
            if !models.contains(default_model) {
                models.insert(0, default_model.clone());
            }
        }
        for alias_name in &aliases {
            if let Some((resolved, _)) = llm_config::resolve_tier_model(alias_name, Some(&name)) {
                if !models.contains(&resolved)
                    && llm_config::model_catalog_entry_for_route(&name, &resolved)
                        .is_some_and(|model| !model.deprecated)
                {
                    models.push(resolved);
                }
            }
        }
        models.sort();
        models.dedup();
        providers.push(PortalLlmProviderOption {
            name: name.clone(),
            base_url,
            base_url_env: def.base_url_env.clone(),
            auth_style: def.auth_style.clone(),
            auth_envs,
            auth_configured,
            viable,
            local,
            models,
            aliases,
            default_model: default_model.unwrap_or_default(),
        });
    }

    providers.sort_by(|left, right| {
        right
            .viable
            .cmp(&left.viable)
            .then_with(|| right.local.cmp(&left.local))
            .then_with(|| left.name.cmp(&right.name))
    });

    PortalLlmOptions {
        preferred_provider,
        preferred_model,
        providers,
    }
}

fn is_local_provider(provider: &str, base_url: &str) -> bool {
    matches!(provider, "local" | "mlx" | "ollama")
        || url::Url::parse(base_url)
            .ok()
            .and_then(|url| {
                url.host().map(|host| match host {
                    url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
                    url::Host::Ipv4(address) => address.is_loopback(),
                    url::Host::Ipv6(address) => address.is_loopback(),
                })
            })
            .unwrap_or(false)
}

async fn discover_provider_models(
    provider: &str,
    base_url: &str,
    def: &llm_config::ProviderDef,
) -> Result<Vec<String>, String> {
    let client = net::http_client_builder("cli.portal.llm")
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|error| {
            format!(
                "failed to build model discovery client: {}",
                net::reqwest_error(&error)
            )
        })?;

    let response = if provider == "ollama" || def.chat_endpoint.contains("/api/chat") {
        client
            .get(format!("{base_url}/api/tags"))
            .send()
            .await
            .map_err(|error| {
                format!("failed to reach {provider}: {}", net::reqwest_error(&error))
            })?
    } else {
        client
            .get(format!("{base_url}/v1/models"))
            .send()
            .await
            .map_err(|error| {
                format!("failed to reach {provider}: {}", net::reqwest_error(&error))
            })?
    };
    if !response.status().is_success() {
        return Err(format!(
            "failed to discover {provider} models: HTTP {}",
            response.status()
        ));
    }
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("failed to parse model list: {error}"))?;
    let (entries, field) = if provider == "ollama" || def.chat_endpoint.contains("/api/chat") {
        (
            payload.get("models").and_then(|value| value.as_array()),
            "name",
        )
    } else {
        (payload.get("data").and_then(|value| value.as_array()), "id")
    };
    let entries = entries.ok_or_else(|| format!("invalid {provider} model list response"))?;
    let mut models = entries
        .iter()
        .filter_map(|entry| entry.get(field).and_then(|value| value.as_str()))
        .map(str::to_string)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::is_local_provider;

    #[test]
    fn local_model_discovery_uses_provider_identity_or_loopback_host() {
        assert!(is_local_provider("local", "http://192.168.1.40:8000"));
        assert!(is_local_provider("custom", "http://[::1]:8000"));
        assert!(!is_local_provider(
            "openai",
            "https://localhost.example.com/v1"
        ));
    }
}
