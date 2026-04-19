use harn_vm::llm_config;

use super::dto::{PortalLlmOptions, PortalLlmProviderOption};

pub(super) async fn build_llm_options() -> PortalLlmOptions {
    let config = llm_config::load_config();
    let preferred_provider = std::env::var("HARN_LLM_PROVIDER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
                Some("local".to_string())
            } else {
                None
            }
        });
    let preferred_model = std::env::var("HARN_LLM_MODEL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LOCAL_LLM_MODEL")
                .ok()
                .filter(|value| !value.is_empty())
        });

    let mut providers = Vec::new();
    for name in llm_config::provider_names() {
        let Some(def) = llm_config::provider_config(&name) else {
            continue;
        };
        let base_url = llm_config::resolve_base_url(&def);
        let auth_envs = auth_env_names(&def.auth_env);
        let auth_configured = auth_envs.iter().any(|env_name| {
            std::env::var(env_name)
                .ok()
                .is_some_and(|value| !value.is_empty())
        });
        let viable = def.auth_style == "none" || auth_configured;
        let local = is_local_provider(&base_url);
        let aliases = config
            .aliases
            .iter()
            .filter(|(_, alias)| alias.provider == name)
            .map(|(alias_name, _)| alias_name.clone())
            .collect::<Vec<_>>();
        let mut models = if local {
            discover_provider_models(&name, &base_url, &def)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if let Some(default_model) = default_model_for_provider(&name) {
            if !models.contains(&default_model) {
                models.insert(0, default_model.clone());
            }
        }
        for alias_name in &aliases {
            if let Some((resolved, _)) = llm_config::resolve_tier_model(alias_name, Some(&name)) {
                if !models.contains(&resolved) {
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
            default_model: default_model_for_provider(&name).unwrap_or_default(),
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

fn auth_env_names(auth_env: &llm_config::AuthEnv) -> Vec<String> {
    match auth_env {
        llm_config::AuthEnv::None => Vec::new(),
        llm_config::AuthEnv::Single(name) => vec![name.clone()],
        llm_config::AuthEnv::Multiple(names) => names.clone(),
    }
}

fn is_local_provider(base_url: &str) -> bool {
    base_url.contains("127.0.0.1") || base_url.contains("localhost")
}

fn default_model_for_provider(provider: &str) -> Option<String> {
    match provider {
        "local" => std::env::var("LOCAL_LLM_MODEL")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("HARN_LLM_MODEL")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| Some("gpt-4o".to_string())),
        "openai" => Some("gpt-4o".to_string()),
        "ollama" => Some("llama3.2".to_string()),
        "openrouter" => Some("Qwen/Qwen3.5-9B".to_string()),
        "anthropic" => Some("claude-sonnet-4-20250514".to_string()),
        _ => None,
    }
}

async fn discover_provider_models(
    provider: &str,
    base_url: &str,
    def: &llm_config::ProviderDef,
) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|error| format!("failed to build model discovery client: {error}"))?;

    let response = if provider == "ollama" || def.chat_endpoint.contains("/api/chat") {
        client
            .get(format!("{base_url}/api/tags"))
            .send()
            .await
            .map_err(|error| format!("failed to reach {provider}: {error}"))?
    } else {
        client
            .get(format!("{base_url}/v1/models"))
            .send()
            .await
            .map_err(|error| format!("failed to reach {provider}: {error}"))?
    };
    if !response.status().is_success() {
        return Ok(Vec::new());
    }
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("failed to parse model list: {error}"))?;
    let mut models = Vec::new();
    if provider == "ollama" || def.chat_endpoint.contains("/api/chat") {
        if let Some(entries) = payload.get("models").and_then(|value| value.as_array()) {
            for entry in entries {
                if let Some(name) = entry.get("name").and_then(|value| value.as_str()) {
                    models.push(name.to_string());
                }
            }
        }
    } else if let Some(entries) = payload.get("data").and_then(|value| value.as_array()) {
        for entry in entries {
            if let Some(id) = entry.get("id").and_then(|value| value.as_str()) {
                models.push(id.to_string());
            }
        }
    }
    models.sort();
    models.dedup();
    Ok(models)
}
