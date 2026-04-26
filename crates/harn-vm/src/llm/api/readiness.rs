//! OpenAI-compatible model readiness probes.
//!
//! Local llama.cpp/vLLM-style servers expose their loaded model aliases
//! through `/v1/models`. Unlike context-window discovery, this module keeps
//! distinct user-facing failure categories so hosts can surface actionable
//! startup diagnostics before the first chat request.

use crate::llm_config::{self, ProviderDef};

use super::auth::apply_auth_headers;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReadiness {
    pub valid: bool,
    pub category: String,
    pub message: String,
    pub provider: String,
    pub model: String,
    pub url: Option<String>,
    pub status: Option<u16>,
    pub available_models: Vec<String>,
}

impl ModelReadiness {
    fn ok(
        provider: &str,
        model: &str,
        url: &str,
        status: u16,
        available_models: Vec<String>,
    ) -> Self {
        Self {
            valid: true,
            category: "ok".to_string(),
            message: format!("{provider} is reachable and serves model '{model}' at {url}"),
            provider: provider.to_string(),
            model: model.to_string(),
            url: Some(url.to_string()),
            status: Some(status),
            available_models,
        }
    }

    fn error(
        provider: &str,
        model: &str,
        category: &str,
        message: String,
        url: Option<String>,
        status: Option<u16>,
        available_models: Vec<String>,
    ) -> Self {
        Self {
            valid: false,
            category: category.to_string(),
            message,
            provider: provider.to_string(),
            model: model.to_string(),
            url,
            status,
            available_models,
        }
    }
}

pub fn supports_model_readiness_probe(def: &ProviderDef) -> bool {
    let healthcheck_uses_models = def.healthcheck.as_ref().is_some_and(|hc| {
        hc.method.eq_ignore_ascii_case("GET") && {
            hc.path
                .as_deref()
                .is_some_and(|path| path.contains("models"))
                || hc.url.as_deref().is_some_and(|url| url.contains("models"))
        }
    });
    healthcheck_uses_models || def.chat_endpoint.ends_with("/chat/completions")
}

pub fn selected_model_for_provider(provider: &str) -> Option<String> {
    if provider == "local" {
        if let Ok(model) = std::env::var("LOCAL_LLM_MODEL") {
            if !model.trim().is_empty() {
                let (resolved, _) = llm_config::resolve_model(model.trim());
                return Some(resolved);
            }
        }
    }

    let selected_provider = std::env::var("HARN_LLM_PROVIDER")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if selected_provider.as_deref() == Some(provider) {
        if let Ok(model) = std::env::var("HARN_LLM_MODEL") {
            if !model.trim().is_empty() {
                let (resolved, _) = llm_config::resolve_model(model.trim());
                return Some(resolved);
            }
        }
    }

    None
}

pub fn build_models_url(def: &ProviderDef) -> Result<String, String> {
    let raw = models_healthcheck_url(def).unwrap_or_else(|| {
        join_base_and_path(
            &llm_config::resolve_base_url(def),
            &model_path_from_chat_endpoint(&def.chat_endpoint),
        )
    });
    validate_url(&normalize_loopback(&raw))
}

fn models_healthcheck_url(def: &ProviderDef) -> Option<String> {
    let healthcheck = def.healthcheck.as_ref()?;
    if !healthcheck.method.eq_ignore_ascii_case("GET") {
        return None;
    }
    if let Some(url) = healthcheck.url.as_ref() {
        return url.contains("models").then(|| url.clone());
    }
    let path = healthcheck.path.as_deref()?;
    path.contains("models")
        .then(|| join_base_and_path(&llm_config::resolve_base_url(def), path))
}

pub fn parse_model_ids(json: &serde_json::Value) -> Vec<String> {
    if let Some(data) = json.get("data").and_then(|value| value.as_array()) {
        return data
            .iter()
            .filter_map(|entry| entry.get("id").and_then(|value| value.as_str()))
            .map(str::to_string)
            .collect();
    }

    if let Some(models) = json.get("models").and_then(|value| value.as_array()) {
        return models
            .iter()
            .filter_map(|entry| {
                entry
                    .get("id")
                    .or_else(|| entry.get("name"))
                    .and_then(|value| value.as_str())
            })
            .map(str::to_string)
            .collect();
    }

    Vec::new()
}

pub fn model_is_served(available: &[String], model: &str) -> bool {
    available
        .iter()
        .any(|id| id == model || id.starts_with(model))
}

pub async fn probe_openai_compatible_model(
    provider: &str,
    model: &str,
    api_key: &str,
) -> ModelReadiness {
    let Some(def) = llm_config::provider_config(provider) else {
        return ModelReadiness::error(
            provider,
            model,
            "unknown_provider",
            format!("Unknown provider: {provider}"),
            None,
            None,
            Vec::new(),
        );
    };

    probe_openai_compatible_model_with_def(provider, model, api_key, &def).await
}

pub(crate) async fn probe_openai_compatible_model_with_def(
    provider: &str,
    model: &str,
    api_key: &str,
    def: &ProviderDef,
) -> ModelReadiness {
    let url = match build_models_url(def) {
        Ok(url) => url,
        Err(error) => {
            return ModelReadiness::error(
                provider,
                model,
                "invalid_url",
                format!("Invalid OpenAI-compatible models URL for {provider}: {error}"),
                None,
                None,
                Vec::new(),
            );
        }
    };

    let client = crate::llm::shared_utility_client();
    let req = client
        .get(&url)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10));
    let req = apply_auth_headers(req, api_key, Some(def));
    let req = def
        .extra_headers
        .iter()
        .fold(req, |req, (name, value)| req.header(name, value));

    let response = match req.send().await {
        Ok(response) => response,
        Err(error) => {
            return ModelReadiness::error(
                provider,
                model,
                "unreachable",
                format!("{provider} OpenAI-compatible server not reachable at {url}: {error}"),
                Some(url),
                None,
                Vec::new(),
            );
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return ModelReadiness::error(
            provider,
            model,
            "bad_status",
            format!(
                "{provider} returned HTTP {} at {url}: {body}",
                status.as_u16()
            ),
            Some(url),
            Some(status.as_u16()),
            Vec::new(),
        );
    }

    let status_code = status.as_u16();
    let json: serde_json::Value = match response.json().await {
        Ok(json) => json,
        Err(error) => {
            return ModelReadiness::error(
                provider,
                model,
                "invalid_response",
                format!("Could not parse {provider} /models response at {url}: {error}"),
                Some(url),
                Some(status_code),
                Vec::new(),
            );
        }
    };
    let available_models = parse_model_ids(&json);
    if available_models.is_empty() {
        return ModelReadiness::error(
            provider,
            model,
            "invalid_response",
            format!("Could not find model ids in {provider} /models response at {url}"),
            Some(url),
            Some(status_code),
            available_models,
        );
    }

    if !model_is_served(&available_models, model) {
        let available = available_models.join(", ");
        return ModelReadiness::error(
            provider,
            model,
            "model_missing",
            format!(
                "Model '{model}' is not served by {provider} at {url}. Currently served: {available}"
            ),
            Some(url),
            Some(status_code),
            available_models,
        );
    }

    ModelReadiness::ok(provider, model, &url, status_code, available_models)
}

fn model_path_from_chat_endpoint(chat_endpoint: &str) -> String {
    if let Some(prefix) = chat_endpoint.strip_suffix("/chat/completions") {
        if prefix.is_empty() {
            "/models".to_string()
        } else {
            format!("{prefix}/models")
        }
    } else {
        "/models".to_string()
    }
}

fn join_base_and_path(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        base.to_string()
    } else if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

fn normalize_loopback(url: &str) -> String {
    url.replace("://localhost:", "://127.0.0.1:")
}

fn validate_url(url: &str) -> Result<String, String> {
    reqwest::Url::parse(url)
        .map(|_| url.to_string())
        .map_err(|error| format!("{url} ({error})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_config::{HealthcheckDef, ProviderDef};

    #[test]
    fn parses_openai_and_ollama_style_model_ids() {
        let openai = serde_json::json!({
            "data": [{"id": "qwen-alias"}, {"id": "other"}]
        });
        assert_eq!(
            parse_model_ids(&openai),
            vec!["qwen-alias".to_string(), "other".to_string()]
        );

        let models = serde_json::json!({
            "models": [{"name": "llama"}, {"id": "qwen"}]
        });
        assert_eq!(
            parse_model_ids(&models),
            vec!["llama".to_string(), "qwen".to_string()]
        );
    }

    #[test]
    fn model_matching_accepts_exact_or_prefix() {
        let ids = vec![
            "qwen36".to_string(),
            "gpt-oss:20b".to_string(),
            "llama-local-long-id".to_string(),
        ];
        assert!(model_is_served(&ids, "qwen36"));
        assert!(model_is_served(&ids, "llama-local"));
        assert!(!model_is_served(&ids, "missing"));
    }

    #[test]
    fn models_url_uses_healthcheck_path_and_loopback_normalization() {
        let def = ProviderDef {
            base_url: "http://localhost:8001".to_string(),
            chat_endpoint: "/v1/chat/completions".to_string(),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/v1/models".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        };

        assert_eq!(
            build_models_url(&def).unwrap(),
            "http://127.0.0.1:8001/v1/models"
        );
    }

    #[test]
    fn models_url_derives_path_from_chat_endpoint() {
        let def = ProviderDef {
            base_url: "http://127.0.0.1:8000".to_string(),
            chat_endpoint: "/v1/chat/completions".to_string(),
            healthcheck: None,
            ..Default::default()
        };

        assert_eq!(
            build_models_url(&def).unwrap(),
            "http://127.0.0.1:8000/v1/models"
        );
    }

    #[test]
    fn models_url_ignores_non_model_healthcheck_path() {
        let def = ProviderDef {
            base_url: "http://127.0.0.1:8080".to_string(),
            chat_endpoint: "/v1/chat/completions".to_string(),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/health".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        };

        assert_eq!(
            build_models_url(&def).unwrap(),
            "http://127.0.0.1:8080/v1/models"
        );
    }

    #[tokio::test]
    async fn probe_reports_ready_when_model_is_served() {
        let def = test_def_with_response(200, r#"{"data":[{"id":"served-model-long"}]}"#).await;

        let result =
            probe_openai_compatible_model_with_def("local", "served-model", "", &def).await;

        assert!(result.valid);
        assert_eq!(result.category, "ok");
        assert_eq!(
            result.available_models,
            vec!["served-model-long".to_string()]
        );
    }

    #[tokio::test]
    async fn probe_distinguishes_model_missing() {
        let def = test_def_with_response(200, r#"{"data":[{"id":"served-model"}]}"#).await;

        let result = probe_openai_compatible_model_with_def("local", "missing", "", &def).await;

        assert!(!result.valid);
        assert_eq!(result.category, "model_missing");
        assert_eq!(result.available_models, vec!["served-model".to_string()]);
    }

    #[tokio::test]
    async fn probe_distinguishes_bad_status() {
        let def = test_def_with_response(503, "loading").await;

        let result =
            probe_openai_compatible_model_with_def("local", "served-model", "", &def).await;

        assert!(!result.valid);
        assert_eq!(result.category, "bad_status");
        assert_eq!(result.status, Some(503));
    }

    #[tokio::test]
    async fn probe_distinguishes_invalid_url() {
        let def = ProviderDef {
            base_url: "not a url".to_string(),
            chat_endpoint: "/v1/chat/completions".to_string(),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/v1/models".to_string()),
                url: None,
                body: None,
            }),
            auth_style: "none".to_string(),
            ..Default::default()
        };

        let result =
            probe_openai_compatible_model_with_def("local", "served-model", "", &def).await;

        assert!(!result.valid);
        assert_eq!(result.category, "invalid_url");
    }

    #[tokio::test]
    async fn probe_distinguishes_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let def = ProviderDef {
            base_url: format!("http://{addr}"),
            chat_endpoint: "/v1/chat/completions".to_string(),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/v1/models".to_string()),
                url: None,
                body: None,
            }),
            auth_style: "none".to_string(),
            ..Default::default()
        };

        let result =
            probe_openai_compatible_model_with_def("local", "served-model", "", &def).await;

        assert!(!result.valid);
        assert_eq!(result.category, "unreachable");
    }

    async fn test_def_with_response(status: u16, body: &'static str) -> ProviderDef {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0_u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                .await
                .expect("write");
        });

        ProviderDef {
            base_url: format!("http://{addr}"),
            chat_endpoint: "/v1/chat/completions".to_string(),
            healthcheck: Some(HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/v1/models".to_string()),
                url: None,
                body: None,
            }),
            auth_style: "none".to_string(),
            ..Default::default()
        }
    }
}
