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

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderReadinessOptions<'a> {
    pub requested_model: Option<&'a str>,
    pub base_url_override: Option<&'a str>,
    pub api_key_override: Option<&'a str>,
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
    configured_model_for_provider(provider).map(|model| {
        let (resolved, _) = llm_config::resolve_model(model.trim());
        resolved
    })
}

pub fn build_models_url(def: &ProviderDef) -> Result<String, String> {
    let base_url = llm_config::resolve_base_url(def);
    models_url(def, &base_url)
}

pub async fn probe_provider_readiness(
    provider: &str,
    requested_model: Option<&str>,
    base_url_override: Option<&str>,
) -> ProviderReadiness {
    probe_provider_readiness_with_options(
        provider,
        ProviderReadinessOptions {
            requested_model,
            base_url_override,
            api_key_override: None,
        },
    )
    .await
}

pub async fn probe_provider_readiness_with_options(
    provider: &str,
    options: ProviderReadinessOptions<'_>,
) -> ProviderReadiness {
    let Some(def) = llm_config::provider_config(provider) else {
        return ProviderReadiness::fail(
            provider,
            ReadinessStatus::UnknownProvider,
            format!("Unknown provider: {provider}"),
            None,
            None,
            options.requested_model.map(ToOwned::to_owned),
            options.requested_model.map(ToOwned::to_owned),
            None,
        );
    };

    let base_url = options
        .base_url_override
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
                options.requested_model.map(ToOwned::to_owned),
                options.requested_model.map(ToOwned::to_owned),
                None,
            );
        }
    };

    let (raw_model, resolved_model) = options
        .requested_model
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
    let api_key = options
        .api_key_override
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| resolve_api_key(provider).unwrap_or_default());
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

    let readiness_model = resolved_model.as_deref().map(llm_config::wire_model_id);

    if let Some(model) = readiness_model.as_deref() {
        if !model_is_served(model, &served_models) {
            let display_model = resolved_model.as_deref().unwrap_or(model);
            let model_label = if display_model == model {
                format!("Model '{display_model}'")
            } else {
                format!("Model '{display_model}' (wire model '{model}')")
            };
            return ProviderReadiness {
                provider: provider.to_string(),
                ok: false,
                status: ReadinessStatus::ModelMissing,
                message: format!(
                    "{model_label} is not served by {provider} at {base_url}. Currently served: {}",
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

    let message = match (resolved_model.as_deref(), readiness_model.as_deref()) {
        (Some(model), Some(wire_model)) if model != wire_model => format!(
            "{provider} is ready at {base_url}; model '{model}' is served as '{wire_model}'"
        ),
        (Some(model), _) => format!("{provider} is ready at {base_url}; model '{model}' is served"),
        (None, _) => format!(
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
    Ok(parse_model_ids_from_value(&payload))
}

pub fn parse_model_ids_from_value(payload: &serde_json::Value) -> Vec<String> {
    let mut models = Vec::new();
    if let Some(entries) = payload.as_array() {
        collect_model_ids(entries, &mut models);
    }
    if let Some(entries) = payload.get("data").and_then(|value| value.as_array()) {
        collect_model_ids(entries, &mut models);
    }
    if let Some(entries) = payload.get("models").and_then(|value| value.as_array()) {
        collect_model_ids(entries, &mut models);
    }
    models.sort();
    models.dedup();
    models
}

fn collect_model_ids(entries: &[serde_json::Value], models: &mut Vec<String>) {
    for entry in entries {
        if let Some(id) = entry.as_str().or_else(|| {
            entry
                .get("id")
                .or_else(|| entry.get("name"))
                .and_then(|value| value.as_str())
        }) {
            models.push(id.to_string());
        }
    }
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
    if let Some(url) = def.healthcheck.as_ref().and_then(|healthcheck| {
        if healthcheck.method.eq_ignore_ascii_case("GET") {
            healthcheck
                .url
                .as_deref()
                .filter(|url| url.contains("models"))
        } else {
            None
        }
    }) {
        return reqwest::Url::parse(url)
            .map(|_| normalize_loopback(url))
            .map_err(|error| format!("Invalid provider models URL '{url}': {error}"));
    }

    let path = def
        .healthcheck
        .as_ref()
        .and_then(|healthcheck| {
            if healthcheck.method.eq_ignore_ascii_case("GET") {
                healthcheck
                    .path
                    .as_deref()
                    .filter(|path| path.contains("models"))
            } else {
                None
            }
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| model_path_from_chat_endpoint(&def.chat_endpoint));
    let url = join_base_and_path(base_url, &path);
    reqwest::Url::parse(&url)
        .map(|_| normalize_loopback(&url))
        .map_err(|error| format!("Invalid provider models URL '{url}': {error}"))
}

fn model_path_from_chat_endpoint(chat_endpoint: &str) -> String {
    if let Some(prefix) = chat_endpoint.strip_suffix("/chat/completions") {
        if prefix.is_empty() {
            "/models".to_string()
        } else {
            format!("{prefix}/models")
        }
    } else {
        "/v1/models".to_string()
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
    fn parse_model_ids_reads_together_top_level_array() {
        let models = parse_model_ids(r#"[{"id":"deepseek-ai/DeepSeek-V4-Pro"},{"name":"qwen"}]"#)
            .expect("parse models");
        assert_eq!(
            models,
            vec![
                "deepseek-ai/DeepSeek-V4-Pro".to_string(),
                "qwen".to_string()
            ]
        );
    }

    #[test]
    fn models_url_does_not_duplicate_version_prefix_in_base_url() {
        let def = ProviderDef {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            chat_endpoint: "/chat/completions".to_string(),
            healthcheck: Some(crate::llm_config::HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/auth/key".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        };

        assert_eq!(
            models_url(&def, &def.base_url).expect("models url"),
            "https://openrouter.ai/api/v1/models"
        );
    }

    #[test]
    fn models_url_keeps_openai_compatible_fallback_for_native_chat_endpoint() {
        let def = ProviderDef {
            base_url: "http://localhost:11434".to_string(),
            chat_endpoint: "/api/chat".to_string(),
            healthcheck: Some(crate::llm_config::HealthcheckDef {
                method: "GET".to_string(),
                path: Some("/api/tags".to_string()),
                url: None,
                body: None,
            }),
            ..Default::default()
        };

        assert_eq!(
            models_url(&def, &def.base_url).expect("models url"),
            "http://127.0.0.1:11434/v1/models"
        );
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
    async fn probe_provider_readiness_verifies_wire_model_for_catalog_key() {
        let (base_url, handle) = spawn_models_stub(200, r#"{"data":[{"id":"zai-org/GLM-5.2"}]}"#);
        let result = probe_provider_readiness(
            "deepinfra",
            Some("deepinfra/zai-org/GLM-5.2"),
            Some(&base_url),
        )
        .await;
        handle.join().expect("stub joins");
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.status, ReadinessStatus::Ok);
        assert_eq!(result.model.as_deref(), Some("deepinfra/zai-org/GLM-5.2"));
        assert!(result.message.contains("served as 'zai-org/GLM-5.2'"));
    }

    #[tokio::test]
    async fn probe_provider_readiness_uses_explicit_api_key_override() {
        let (base_url, handle) = spawn_models_stub_with_expected_header(
            200,
            r#"{"data":[{"id":"zai-org/GLM-5.2"}]}"#,
            Some("authorization: Bearer test-key"),
        );
        let result = probe_provider_readiness_with_options(
            "deepinfra",
            ProviderReadinessOptions {
                requested_model: Some("deepinfra/zai-org/GLM-5.2"),
                base_url_override: Some(&base_url),
                api_key_override: Some("test-key"),
            },
        )
        .await;
        handle.join().expect("stub joins");
        assert!(result.ok, "{}", result.message);
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
        spawn_models_stub_with_expected_header(status, body, None)
    }

    fn spawn_models_stub_with_expected_header(
        status: u16,
        body: &'static str,
        expected_header: Option<&'static str>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind models stub");
        let addr = listener.local_addr().expect("stub addr");
        // Block on `accept()` directly rather than polling. The
        // earlier 3s wall-clock deadline + 20ms polling sleep was
        // brittle under nextest's flake-detection profile: another
        // concurrent test could starve this thread of CPU long
        // enough for the deadline to elapse before the kernel even
        // delivered the SYN that the client had already sent.
        // Blocking accept is deterministic; the test invariably
        // sends a request, so it returns promptly.
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .unwrap_or_else(|e| panic!("models stub: accept failed: {e}"));
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(
                request.starts_with("GET /v1/models HTTP/1.1\r\n")
                    || request.starts_with("GET /models HTTP/1.1\r\n")
            );
            if let Some(header) = expected_header {
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case(header)),
                    "expected request header {header:?}, got:\n{request}"
                );
            }
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
