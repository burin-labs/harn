//! Ollama-specific runtime settings consumed by chat, completion, and
//! model warmup paths.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct OllamaWarmupResult {
    pub valid: bool,
    pub status: String,
    pub message: String,
    pub url: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OllamaReadinessResult {
    pub valid: bool,
    pub status: String,
    pub message: String,
    pub base_url: String,
    pub tags_url: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_model: Option<String>,
    pub available_models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warmup: Option<OllamaWarmupResult>,
}

#[derive(Debug, Clone)]
pub struct OllamaReadinessOptions {
    pub model: String,
    pub base_url: Option<String>,
    pub warm: bool,
    pub keep_alive: Option<serde_json::Value>,
    pub tags_timeout: Duration,
    pub warmup_timeout: Duration,
}

impl OllamaReadinessOptions {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            base_url: None,
            warm: false,
            keep_alive: None,
            tags_timeout: Duration::from_secs(15),
            warmup_timeout: Duration::from_secs(135),
        }
    }
}

/// Public wrapper around the internal keep-alive parser, used by callers
/// (CLI flags, host bridges) that want the same normalization Harn applies
/// to environment overrides.
pub fn normalize_ollama_keep_alive(raw: &str) -> Option<serde_json::Value> {
    parse_keep_alive_str(raw)
}

/// Resolve the keep-alive override from `HARN_OLLAMA_KEEP_ALIVE` /
/// `OLLAMA_KEEP_ALIVE`, normalized through [`normalize_ollama_keep_alive`].
pub fn ollama_keep_alive_override() -> Option<serde_json::Value> {
    keep_alive_from_env()
}

pub const OLLAMA_DEFAULT_NUM_CTX: u64 = 32_768;
pub const OLLAMA_DEFAULT_KEEP_ALIVE: &str = "30m";
pub const OLLAMA_DEFAULT_UNLOAD_GRACE_MS: u64 = 10_000;
pub const HARN_OLLAMA_NUM_CTX_ENV: &str = "HARN_OLLAMA_NUM_CTX";
pub const HARN_OLLAMA_KEEP_ALIVE_ENV: &str = "HARN_OLLAMA_KEEP_ALIVE";
pub const HARN_OLLAMA_UNLOAD_GRACE_MS_ENV: &str = "HARN_OLLAMA_UNLOAD_GRACE_MS";
pub const OLLAMA_UNLOAD_GRACE_MS_ENV: &str = "OLLAMA_UNLOAD_GRACE_MS";
pub const OLLAMA_HOST_ENV: &str = "OLLAMA_HOST";

const OLLAMA_NUM_CTX_ENV_KEYS: [&str; 3] = [
    HARN_OLLAMA_NUM_CTX_ENV,
    "OLLAMA_CONTEXT_LENGTH",
    "OLLAMA_NUM_CTX",
];
const OLLAMA_KEEP_ALIVE_ENV_KEYS: [&str; 2] = [HARN_OLLAMA_KEEP_ALIVE_ENV, "OLLAMA_KEEP_ALIVE"];
const OLLAMA_UNLOAD_GRACE_MS_ENV_KEYS: [&str; 2] =
    [HARN_OLLAMA_UNLOAD_GRACE_MS_ENV, OLLAMA_UNLOAD_GRACE_MS_ENV];
const OLLAMA_DEFAULT_BASE_URL: &str = "http://localhost:11434";

#[derive(Clone, Debug, PartialEq)]
pub struct OllamaRuntimeSettings {
    pub num_ctx: u64,
    pub keep_alive: Value,
}

impl OllamaRuntimeSettings {
    pub fn from_env() -> Self {
        Self::from_env_and_overrides(None)
    }

    pub fn from_env_and_overrides(overrides: Option<&Value>) -> Self {
        Self {
            num_ctx: num_ctx_from_overrides(overrides)
                .or_else(num_ctx_from_env)
                .unwrap_or(OLLAMA_DEFAULT_NUM_CTX),
            keep_alive: keep_alive_from_overrides(overrides)
                .or_else(keep_alive_from_env)
                .unwrap_or_else(default_keep_alive_value),
        }
    }

    pub fn warmup_body(&self, model: &str) -> Value {
        serde_json::json!({
            "model": model,
            "prompt": "",
            "stream": false,
            "keep_alive": self.keep_alive,
            "options": {
                "num_ctx": self.num_ctx,
            },
        })
    }
}

pub fn ollama_runtime_settings_from_env() -> OllamaRuntimeSettings {
    OllamaRuntimeSettings::from_env()
}

pub(crate) fn ollama_unload_grace_duration_from_env() -> Duration {
    Duration::from_millis(
        OLLAMA_UNLOAD_GRACE_MS_ENV_KEYS
            .iter()
            .find_map(|key| std::env::var(key).ok().and_then(|raw| parse_grace_ms(&raw)))
            .unwrap_or(OLLAMA_DEFAULT_UNLOAD_GRACE_MS),
    )
}

pub async fn warm_ollama_model(model: &str, base_url: Option<&str>) -> Result<(), String> {
    let settings = OllamaRuntimeSettings::from_env();
    warm_ollama_model_with_settings(model, base_url, &settings).await
}

pub async fn warm_ollama_model_with_settings(
    model: &str,
    base_url: Option<&str>,
    settings: &OllamaRuntimeSettings,
) -> Result<(), String> {
    let base_url = resolve_ollama_base_url(base_url);
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let response = crate::llm::shared_utility_client()
        .post(url)
        .header("Content-Type", "application/json")
        .json(&settings.warmup_body(model))
        .send()
        .await
        .map_err(|error| format!("Ollama warmup failed: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(format!("Ollama warmup returned HTTP {status}: {body}"))
    }
}

pub(crate) fn apply_ollama_runtime_settings(body: &mut Value, overrides: Option<&Value>) {
    apply_non_runtime_ollama_overrides(body, overrides);

    let explicit_num_ctx = num_ctx_from_overrides(overrides);
    if explicit_num_ctx.is_some() || body.pointer("/options/num_ctx").is_none() {
        let num_ctx = explicit_num_ctx
            .or_else(num_ctx_from_env)
            .unwrap_or(OLLAMA_DEFAULT_NUM_CTX);
        ensure_options_object(body).insert("num_ctx".to_string(), serde_json::json!(num_ctx));
    }

    let explicit_keep_alive = keep_alive_from_overrides(overrides);
    if let Some(keep_alive) = explicit_keep_alive
        .or_else(|| body.get("keep_alive").cloned())
        .or_else(keep_alive_from_env)
        .or_else(|| Some(default_keep_alive_value()))
    {
        body["keep_alive"] = keep_alive;
    }
}

fn resolve_ollama_base_url(base_url: Option<&str>) -> String {
    base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(OLLAMA_HOST_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| OLLAMA_DEFAULT_BASE_URL.to_string())
}

fn num_ctx_from_env() -> Option<u64> {
    OLLAMA_NUM_CTX_ENV_KEYS
        .iter()
        .find_map(|key| std::env::var(key).ok().and_then(|raw| parse_num_ctx(&raw)))
}

fn keep_alive_from_env() -> Option<Value> {
    OLLAMA_KEEP_ALIVE_ENV_KEYS.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .and_then(|raw| parse_keep_alive_str(&raw))
    })
}

fn num_ctx_from_overrides(overrides: Option<&Value>) -> Option<u64> {
    let obj = overrides?.as_object()?;
    obj.get("num_ctx")
        .and_then(parse_num_ctx_value)
        .or_else(|| {
            obj.get("options")
                .and_then(|options| options.get("num_ctx"))
                .and_then(parse_num_ctx_value)
        })
}

fn keep_alive_from_overrides(overrides: Option<&Value>) -> Option<Value> {
    overrides?
        .as_object()?
        .get("keep_alive")
        .and_then(parse_keep_alive_value)
}

fn parse_num_ctx(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok().filter(|parsed| *parsed > 0)
}

fn parse_grace_ms(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok()
}

fn parse_num_ctx_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().filter(|parsed| *parsed > 0),
        Value::String(raw) => parse_num_ctx(raw),
        _ => None,
    }
}

fn parse_keep_alive_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(raw) => parse_keep_alive_str(raw),
        Value::Number(_) => Some(value.clone()),
        _ => None,
    }
}

fn parse_keep_alive_str(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(match trimmed.to_ascii_lowercase().as_str() {
        "default" => default_keep_alive_value(),
        "forever" | "infinite" | "-1" => serde_json::json!(-1),
        _ => {
            if let Ok(n) = trimmed.parse::<i64>() {
                serde_json::json!(n)
            } else {
                serde_json::json!(trimmed)
            }
        }
    })
}

fn default_keep_alive_value() -> Value {
    serde_json::json!(OLLAMA_DEFAULT_KEEP_ALIVE)
}

fn ensure_options_object(body: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !body.get("options").is_some_and(Value::is_object) {
        body["options"] = serde_json::json!({});
    }
    body["options"]
        .as_object_mut()
        .expect("options initialized as object")
}

fn apply_non_runtime_ollama_overrides(body: &mut Value, overrides: Option<&Value>) {
    let Some(obj) = overrides.and_then(Value::as_object) else {
        return;
    };

    for (key, value) in obj {
        match key.as_str() {
            "num_ctx" | "keep_alive" => {}
            "options" => {
                if let Some(options) = value.as_object() {
                    let body_options = ensure_options_object(body);
                    for (option_key, option_value) in options {
                        if option_key != "num_ctx" {
                            body_options.insert(option_key.clone(), option_value.clone());
                        }
                    }
                }
            }
            _ => {
                body[key] = value.clone();
            }
        }
    }
}

pub async fn ollama_readiness(options: OllamaReadinessOptions) -> OllamaReadinessResult {
    let base_url = options.base_url.unwrap_or_else(default_ollama_base_url);
    let tags_url = match ollama_endpoint_url(&base_url, "/api/tags") {
        Ok(url) => url,
        Err(message) => {
            return OllamaReadinessResult {
                valid: false,
                status: "invalid_url".to_string(),
                message,
                base_url,
                tags_url: String::new(),
                model: options.model,
                matched_model: None,
                available_models: Vec::new(),
                http_status: None,
                keep_alive: None,
                warmup: None,
            };
        }
    };

    let client = crate::llm::shared_utility_client();
    let response = match client
        .get(tags_url.clone())
        .timeout(options.tags_timeout)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return OllamaReadinessResult {
                valid: false,
                status: "daemon_down".to_string(),
                message: format!("Ollama not reachable at {tags_url}: {error}"),
                base_url,
                tags_url,
                model: options.model,
                matched_model: None,
                available_models: Vec::new(),
                http_status: None,
                keep_alive: None,
                warmup: None,
            };
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return OllamaReadinessResult {
            valid: false,
            status: "bad_status".to_string(),
            message: format!(
                "Ollama returned HTTP {} from /api/tags: {}",
                status.as_u16(),
                body
            ),
            base_url,
            tags_url,
            model: options.model,
            matched_model: None,
            available_models: Vec::new(),
            http_status: Some(status.as_u16()),
            keep_alive: None,
            warmup: None,
        };
    }

    let body: serde_json::Value = match response.json().await {
        Ok(value) => value,
        Err(error) => {
            return OllamaReadinessResult {
                valid: false,
                status: "invalid_response".to_string(),
                message: format!("Could not parse Ollama model list: {error}"),
                base_url,
                tags_url,
                model: options.model,
                matched_model: None,
                available_models: Vec::new(),
                http_status: Some(status.as_u16()),
                keep_alive: None,
                warmup: None,
            };
        }
    };

    let Some(models) = parse_ollama_model_names(&body) else {
        return OllamaReadinessResult {
            valid: false,
            status: "invalid_response".to_string(),
            message: "Could not parse Ollama model list: missing models[].name".to_string(),
            base_url,
            tags_url,
            model: options.model,
            matched_model: None,
            available_models: Vec::new(),
            http_status: Some(status.as_u16()),
            keep_alive: None,
            warmup: None,
        };
    };

    let matched_model = find_ollama_model_match(&models, &options.model);
    let Some(matched) = matched_model.clone() else {
        let available = if models.is_empty() {
            "(none)".to_string()
        } else {
            models.join(", ")
        };
        return OllamaReadinessResult {
            valid: false,
            status: "model_missing".to_string(),
            message: format!(
                "Ollama model '{}' not found. Available: {available}",
                options.model
            ),
            base_url,
            tags_url,
            model: options.model,
            matched_model: None,
            available_models: models,
            http_status: Some(status.as_u16()),
            keep_alive: None,
            warmup: None,
        };
    };

    let keep_alive = options
        .keep_alive
        .or_else(ollama_keep_alive_override)
        .or_else(|| Some(serde_json::json!("30m")));
    let mut warmup = None;
    let mut valid = true;
    let mut readiness_status = "ok".to_string();
    let mut message = format!("Ollama is reachable and model '{matched}' is available");

    if options.warm {
        let warm = ollama_warmup(
            &base_url,
            &matched,
            keep_alive.clone(),
            options.warmup_timeout,
        )
        .await;
        if !warm.valid {
            valid = false;
            readiness_status = "warmup_failed".to_string();
            message = warm.message.clone();
        } else {
            message = format!("{message}; {}", warm.message);
        }
        warmup = Some(warm);
    }

    OllamaReadinessResult {
        valid,
        status: readiness_status,
        message,
        base_url,
        tags_url,
        model: options.model,
        matched_model: Some(matched),
        available_models: models,
        http_status: Some(status.as_u16()),
        keep_alive,
        warmup,
    }
}

fn default_ollama_base_url() -> String {
    crate::llm_config::provider_config("ollama")
        .as_ref()
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "http://localhost:11434".to_string())
}

fn ollama_endpoint_url(base_url: &str, path: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|error| format!("Invalid Ollama URL '{base_url}': {error}"))?;
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| format!("Invalid Ollama URL '{base_url}': could not normalize host"))?;
    }
    let base_path = url.path().trim_end_matches('/');
    let suffix = path.trim_start_matches('/');
    let joined = if base_path.is_empty() {
        format!("/{suffix}")
    } else {
        format!("{base_path}/{suffix}")
    };
    url.set_path(&joined);
    url.set_query(None);
    Ok(url.to_string())
}

fn parse_ollama_model_names(value: &serde_json::Value) -> Option<Vec<String>> {
    let models = value.get("models")?.as_array()?;
    Some(
        models
            .iter()
            .filter_map(|model| model.get("name").and_then(|name| name.as_str()))
            .map(str::to_string)
            .collect(),
    )
}

fn find_ollama_model_match(models: &[String], selected: &str) -> Option<String> {
    models
        .iter()
        .find(|name| name.as_str() == selected)
        .or_else(|| {
            models
                .iter()
                .find(|name| name.strip_suffix(":latest") == Some(selected))
        })
        .or_else(|| models.iter().find(|name| name.starts_with(selected)))
        .cloned()
}

async fn ollama_warmup(
    base_url: &str,
    model: &str,
    keep_alive: Option<serde_json::Value>,
    timeout: Duration,
) -> OllamaWarmupResult {
    let url = match ollama_endpoint_url(base_url, "/api/generate") {
        Ok(url) => url,
        Err(message) => {
            return OllamaWarmupResult {
                valid: false,
                status: "invalid_url".to_string(),
                message,
                url: String::new(),
                model: model.to_string(),
                http_status: None,
            };
        }
    };

    let mut body = serde_json::json!({
        "model": model,
        "prompt": "",
        "stream": false,
    });
    if let Some(value) = keep_alive {
        body["keep_alive"] = value;
    }

    let client = crate::llm::shared_blocking_client();
    let response = match client
        .post(url.clone())
        .header("Content-Type", "application/json")
        .timeout(timeout)
        .json(&body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return OllamaWarmupResult {
                valid: false,
                status: "warmup_failed".to_string(),
                message: format!("Ollama warmup failed for model '{model}' at {url}: {error}"),
                url,
                model: model.to_string(),
                http_status: None,
            };
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return OllamaWarmupResult {
            valid: false,
            status: "warmup_failed".to_string(),
            message: format!(
                "Ollama warmup returned HTTP {} for model '{model}': {body}",
                status.as_u16()
            ),
            url,
            model: model.to_string(),
            http_status: Some(status.as_u16()),
        };
    }

    let body: serde_json::Value = response.json().await.unwrap_or_default();
    if let Some(error) = body.get("error").and_then(|error| error.as_str()) {
        return OllamaWarmupResult {
            valid: false,
            status: "warmup_failed".to_string(),
            message: format!("Ollama warmup failed for model '{model}': {error}"),
            url,
            model: model.to_string(),
            http_status: Some(status.as_u16()),
        };
    }

    OllamaWarmupResult {
        valid: true,
        status: "ok".to_string(),
        message: format!("Ollama model '{model}' warmed"),
        url,
        model: model.to_string(),
        http_status: Some(status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::env_lock;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn runtime_settings_use_harn_env_before_ollama_env() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "131072"),
            ScopedEnvVar::set("OLLAMA_CONTEXT_LENGTH", "32768"),
            ScopedEnvVar::set("HARN_OLLAMA_KEEP_ALIVE", "forever"),
            ScopedEnvVar::set("OLLAMA_KEEP_ALIVE", "5m"),
        ];
        let settings = OllamaRuntimeSettings::from_env();
        assert_eq!(settings.num_ctx, 131072);
        assert_eq!(settings.keep_alive, serde_json::json!(-1));
    }

    #[test]
    fn runtime_settings_apply_harn_defaults() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::remove("HARN_OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        let settings = OllamaRuntimeSettings::from_env();
        assert_eq!(settings.num_ctx, OLLAMA_DEFAULT_NUM_CTX);
        assert_eq!(settings.keep_alive, serde_json::json!("30m"));
    }

    #[test]
    fn provider_overrides_beat_env_and_normalize_keep_alive() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "131072"),
            ScopedEnvVar::set("HARN_OLLAMA_KEEP_ALIVE", "5m"),
        ];
        let overrides = serde_json::json!({
            "num_ctx": "65536",
            "keep_alive": "infinite",
        });
        let settings = OllamaRuntimeSettings::from_env_and_overrides(Some(&overrides));
        assert_eq!(settings.num_ctx, 65536);
        assert_eq!(settings.keep_alive, serde_json::json!(-1));
    }

    #[test]
    fn apply_runtime_settings_maps_ollama_overrides_to_native_shape() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::remove("HARN_OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        let mut body = serde_json::json!({
            "model": "qwen",
            "options": {"temperature": 0.1}
        });
        let overrides = serde_json::json!({
            "num_ctx": 65536,
            "keep_alive": "default",
            "options": {"top_k": 20, "num_ctx": 999},
            "think": true,
        });
        apply_ollama_runtime_settings(&mut body, Some(&overrides));
        assert_eq!(body["options"]["num_ctx"], serde_json::json!(65536));
        assert_eq!(body["options"]["top_k"], serde_json::json!(20));
        assert_eq!(body["options"]["temperature"], serde_json::json!(0.1));
        assert_eq!(body["keep_alive"], serde_json::json!("30m"));
        assert_eq!(body["think"], serde_json::json!(true));
        assert!(body.get("num_ctx").is_none());
    }

    #[test]
    fn ollama_keep_alive_normalization_handles_default_and_numbers() {
        assert_eq!(
            normalize_ollama_keep_alive("default"),
            Some(serde_json::json!("30m"))
        );
        assert_eq!(
            normalize_ollama_keep_alive("forever"),
            Some(serde_json::json!(-1))
        );
        assert_eq!(
            normalize_ollama_keep_alive("120"),
            Some(serde_json::json!(120))
        );
        assert_eq!(
            normalize_ollama_keep_alive("10m"),
            Some(serde_json::json!("10m"))
        );
        assert_eq!(normalize_ollama_keep_alive("   "), None);
    }

    #[test]
    fn ollama_readiness_verifies_model_and_warms_matched_tag() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (addr, server) = spawn_stub(
            vec![
                (
                    200,
                    r#"{"models":[{"name":"qwen3:latest"},{"name":"llama3.2:latest"}]}"#,
                ),
                (200, r#"{"response":"","done":true}"#),
            ],
            captured.clone(),
        );

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(ollama_readiness(OllamaReadinessOptions {
                model: "qwen3".to_string(),
                base_url: Some(format!("http://{addr}")),
                warm: true,
                keep_alive: Some(serde_json::json!(-1)),
                tags_timeout: Duration::from_secs(2),
                warmup_timeout: Duration::from_secs(2),
            }));

        server.join().expect("stub server");
        assert!(result.valid, "result was: {result:?}");
        assert_eq!(result.status, "ok");
        assert_eq!(result.matched_model.as_deref(), Some("qwen3:latest"));
        assert!(result.warmup.as_ref().is_some_and(|warm| warm.valid));

        let requests = captured.lock().expect("captured requests");
        assert!(requests[0].starts_with("GET /api/tags "));
        assert!(requests[1].starts_with("POST /api/generate "));
        let body = requests[1].split("\r\n\r\n").nth(1).unwrap_or("");
        let json: serde_json::Value = serde_json::from_str(body).expect("warmup body");
        assert_eq!(json["model"], "qwen3:latest");
        assert_eq!(json["prompt"], "");
        assert_eq!(json["stream"], false);
        assert_eq!(json["keep_alive"], -1);
    }

    #[test]
    fn ollama_readiness_reports_missing_model_with_available_tags() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (addr, server) = spawn_stub(
            vec![(200, r#"{"models":[{"name":"llama3.2:latest"}]}"#)],
            captured,
        );

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(ollama_readiness(OllamaReadinessOptions {
                model: "qwen3".to_string(),
                base_url: Some(format!("http://{addr}")),
                warm: false,
                keep_alive: None,
                tags_timeout: Duration::from_secs(2),
                warmup_timeout: Duration::from_secs(2),
            }));

        server.join().expect("stub server");
        assert!(!result.valid);
        assert_eq!(result.status, "model_missing");
        assert_eq!(result.available_models, vec!["llama3.2:latest"]);
        assert!(result.message.contains("qwen3"));
    }

    fn spawn_stub(
        responses: Vec<(u16, &'static str)>,
        captured: Arc<Mutex<Vec<String>>>,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ollama stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                let request = read_http_request(&mut stream);
                captured.lock().expect("captured").push(request);
                let reason = if status == 200 { "OK" } else { "ERROR" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        (addr, handle)
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut data = Vec::new();
        let mut buf = [0_u8; 512];
        loop {
            let n = stream.read(&mut buf).expect("read request");
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&data);
            if let Some(header_end) = text.find("\r\n\r\n") {
                let headers = &text[..header_end];
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if data.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8(data).expect("utf8 request")
    }
}
