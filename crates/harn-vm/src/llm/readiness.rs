use serde::Serialize;

use super::api::apply_auth_headers;
use super::helpers::resolve_api_key;
use crate::llm_config::{self, ProviderDef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ok,
    UnknownProvider,
    InvalidUrl,
    Unreachable,
    BadStatus,
    BadResponse,
    ModelMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderReadiness {
    pub provider: String,
    pub ok: bool,
    pub status: ReadinessStatus,
    pub message: String,
    pub base_url: Option<String>,
    pub url: Option<String>,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub served_models: Vec<String>,
    pub http_status: Option<u16>,
}

impl ProviderReadiness {
    fn fail(
        provider: &str,
        status: ReadinessStatus,
        message: String,
        base_url: Option<String>,
        url: Option<String>,
        model: Option<String>,
        requested_model: Option<String>,
        http_status: Option<u16>,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            ok: false,
            status,
            message,
            base_url,
            url,
            model,
            requested_model,
            served_models: Vec::new(),
            http_status,
        }
    }
}

pub async fn probe_provider_readiness(
    provider: &str,
    requested_model: Option<&str>,
    base_url_override: Option<&str>,
) -> ProviderReadiness {
    let Some(def) = llm_config::provider_config(provider) else {
        return ProviderReadiness::fail(
            provider,
            ReadinessStatus::UnknownProvider,
            format!("Unknown provider: {provider}"),
            None,
            None,
            requested_model.map(ToOwned::to_owned),
            requested_model.map(ToOwned::to_owned),
            None,
        );
    };

    let base_url = base_url_override
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| llm_config::resolve_base_url(&def));
    let url = match models_url(&def, &base_url) {
        Ok(url) => url,
        Err(message) => {
            return ProviderReadiness::fail(
                provider,
                ReadinessStatus::InvalidUrl,
                message,
                Some(base_url),
                None,
                requested_model.map(ToOwned::to_owned),
                requested_model.map(ToOwned::to_owned),
                None,
            );
        }
    };

    let (raw_model, resolved_model) = requested_model
        .filter(|model| !model.trim().is_empty())
        .map(|model| {
            let trimmed = model.trim();
            let (resolved, _) = llm_config::resolve_model(trimmed);
            (Some(trimmed.to_string()), Some(resolved))
        })
        .unwrap_or_else(|| match configured_model_for_provider(provider) {
            Some(model) => {
                let (resolved, _) = llm_config::resolve_model(&model);
                (Some(model), Some(resolved))
            }
            None => (None, None),
        });

    let client = super::shared_utility_client();
    let api_key = resolve_api_key(provider).unwrap_or_default();
    let request = client.get(&url).header("Content-Type", "application/json");
    let request = apply_auth_headers(request, &api_key, Some(&def));
    let request = def
        .extra_headers
        .iter()
        .fold(request, |request, (name, value)| {
            request.header(name.as_str(), value.as_str())
        });

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return ProviderReadiness::fail(
                provider,
                ReadinessStatus::Unreachable,
                format!("{provider} server is not reachable at {base_url}: {error}"),
                Some(base_url),
                Some(url),
                resolved_model,
                raw_model,
                None,
            );
        }
    };

    let http_status = response.status().as_u16();
    if !response.status().is_success() {
        return ProviderReadiness::fail(
            provider,
            ReadinessStatus::BadStatus,
            format!("{provider} returned HTTP {http_status} at {url}"),
            Some(base_url),
            Some(url),
            resolved_model,
            raw_model,
            Some(http_status),
        );
    }

    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            return ProviderReadiness::fail(
                provider,
                ReadinessStatus::BadResponse,
                format!("{provider} returned an unreadable /models response: {error}"),
                Some(base_url),
                Some(url),
                resolved_model,
                raw_model,
                Some(http_status),
            );
        }
    };
    let served_models = match parse_model_ids(&body) {
        Ok(models) if !models.is_empty() => models,
        Ok(_) => {
            return ProviderReadiness::fail(
                provider,
                ReadinessStatus::BadResponse,
                format!("{provider} /models response did not include any model ids"),
                Some(base_url),
                Some(url),
                resolved_model,
                raw_model,
                Some(http_status),
            );
        }
        Err(error) => {
            return ProviderReadiness::fail(
                provider,
                ReadinessStatus::BadResponse,
                format!("{provider} returned an unparsable /models response: {error}"),
                Some(base_url),
                Some(url),
                resolved_model,
                raw_model,
                Some(http_status),
            );
        }
    };

    if let Some(model) = resolved_model.as_deref() {
        if !model_is_served(model, &served_models) {
            return ProviderReadiness {
                provider: provider.to_string(),
                ok: false,
                status: ReadinessStatus::ModelMissing,
                message: format!(
                    "Model '{model}' is not served by {provider} at {base_url}. Currently served: {}",
                    served_models.join(", ")
                ),
                base_url: Some(base_url),
                url: Some(url),
                model: resolved_model,
                requested_model: raw_model,
                served_models,
                http_status: Some(http_status),
            };
        }
    }

    let message = match resolved_model.as_deref() {
        Some(model) => format!("{provider} is ready at {base_url}; model '{model}' is served"),
        None => format!(
            "{provider} is reachable at {base_url}; served models: {}",
            served_models.join(", ")
        ),
    };

    ProviderReadiness {
        provider: provider.to_string(),
        ok: true,
        status: ReadinessStatus::Ok,
        message,
        base_url: Some(base_url),
        url: Some(url),
        model: resolved_model,
        requested_model: raw_model,
        served_models,
        http_status: Some(http_status),
    }
}

pub fn parse_model_ids(body: &str) -> Result<Vec<String>, serde_json::Error> {
    let payload: serde_json::Value = serde_json::from_str(body)?;
    let mut models = Vec::new();
    if let Some(entries) = payload.get("data").and_then(|value| value.as_array()) {
        for entry in entries {
            if let Some(id) = entry.get("id").and_then(|value| value.as_str()) {
                models.push(id.to_string());
            }
        }
    }
    if let Some(entries) = payload.get("models").and_then(|value| value.as_array()) {
        for entry in entries {
            if let Some(id) = entry
                .get("id")
                .or_else(|| entry.get("name"))
                .and_then(|value| value.as_str())
            {
                models.push(id.to_string());
            }
        }
    }
    models.sort();
    models.dedup();
    Ok(models)
}

pub fn model_is_served(model: &str, served_models: &[String]) -> bool {
    served_models
        .iter()
        .any(|served| served == model || served.starts_with(model))
}

pub fn configured_model_for_provider(provider: &str) -> Option<String> {
    if provider == "mlx" {
        if let Ok(model) = std::env::var("MLX_MODEL_ID") {
            if !model.trim().is_empty() {
                return Some(model);
            }
        }
    }
    if provider == "local" {
        if let Ok(model) = std::env::var("LOCAL_LLM_MODEL") {
            if !model.trim().is_empty() {
                return Some(model);
            }
        }
    }
    let harn_provider = std::env::var("HARN_LLM_PROVIDER").ok();
    let model = std::env::var("HARN_LLM_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty())?;
    let (_, resolved_provider) = llm_config::resolve_model(&model);
    if resolved_provider.as_deref() == Some(provider)
        || (resolved_provider.is_none() && harn_provider.as_deref() == Some(provider))
    {
        return Some(model);
    }
    None
}

fn models_url(def: &ProviderDef, base_url: &str) -> Result<String, String> {
    let path = def
        .healthcheck
        .as_ref()
        .and_then(|healthcheck| {
            if healthcheck.method.eq_ignore_ascii_case("GET") {
                healthcheck
                    .path
                    .as_deref()
                    .filter(|path| path.ends_with("/models") || *path == "/models")
            } else {
                None
            }
        })
        .unwrap_or("/v1/models");
    let url = if path.starts_with('/') {
        format!("{}{}", base_url.trim_end_matches('/'), path)
    } else {
        format!("{}/{}", base_url.trim_end_matches('/'), path)
    };
    reqwest::Url::parse(&url)
        .map(|_| url.clone())
        .map_err(|error| format!("Invalid provider models URL '{url}': {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn parse_model_ids_reads_openai_compatible_data() {
        let models =
            parse_model_ids(r#"{"object":"list","data":[{"id":"qwen"},{"id":"mlx-model"}]}"#)
                .expect("parse models");
        assert_eq!(models, vec!["mlx-model".to_string(), "qwen".to_string()]);
    }

    #[test]
    fn model_is_served_accepts_exact_or_prefix() {
        let models = vec!["unsloth/Qwen3.6-27B-UD-MLX-4bit".to_string()];
        assert!(model_is_served("unsloth/Qwen3.6-27B-UD-MLX-4bit", &models));
        assert!(model_is_served("unsloth/Qwen3.6", &models));
        assert!(!model_is_served("Qwen/Qwen3.6-27B", &models));
    }

    #[tokio::test]
    async fn probe_provider_readiness_verifies_served_model() {
        let (base_url, handle) = spawn_models_stub(
            200,
            r#"{"data":[{"id":"unsloth/Qwen3.6-27B-UD-MLX-4bit"}]}"#,
        );
        let result = probe_provider_readiness("mlx", Some("mlx-qwen36-27b"), Some(&base_url)).await;
        handle.join().expect("stub joins");
        assert!(result.ok);
        assert_eq!(result.status, ReadinessStatus::Ok);
        assert_eq!(
            result.model.as_deref(),
            Some("unsloth/Qwen3.6-27B-UD-MLX-4bit")
        );
    }

    #[tokio::test]
    async fn probe_provider_readiness_reports_missing_model() {
        let (base_url, handle) = spawn_models_stub(200, r#"{"data":[{"id":"other-model"}]}"#);
        let result = probe_provider_readiness("mlx", Some("mlx-qwen36-27b"), Some(&base_url)).await;
        handle.join().expect("stub joins");
        assert!(!result.ok);
        assert_eq!(result.status, ReadinessStatus::ModelMissing);
        assert!(result.message.contains("Currently served: other-model"));
    }

    fn spawn_models_stub(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind models stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("set listener nonblocking");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            panic!("models stub: no client within 3s");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(error) => panic!("models stub: accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).expect("set stream blocking");
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (format!("http://{addr}"), handle)
    }
}
