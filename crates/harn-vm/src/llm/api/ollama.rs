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
    /// Runtime settings Harn would inject into a request body for this
    /// model, computed from env, provider overrides, and the catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<OllamaExpectedRequest>,
    /// The matched runner reported by `/api/ps`, if the model is currently
    /// loaded. The `context_length` field is the effective context the
    /// loaded runner was started with — this is what `ollama ps` prints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_runner: Option<OllamaLoadedRunner>,
    /// Set when the loaded runner's `context_length` differs from the
    /// `expected.num_ctx` Harn would request. Explains how to reload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_drift: Option<String>,
}

/// The runtime knobs Harn would attach to chat/completion/warmup
/// requests for a given model. Surfaced by readiness so callers can
/// compare it against what `/api/ps` says is actually loaded.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaExpectedRequest {
    pub num_ctx: u64,
    pub keep_alive: Value,
}

/// One entry from `GET /api/ps` — a model the Ollama daemon currently
/// has loaded into memory. `context_length` is the effective context
/// the runner was started with and is fixed for the lifetime of the
/// loaded process; reloading is required to change it.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaLoadedRunner {
    pub name: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_vram: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OllamaReadinessOptions {
    pub model: String,
    pub base_url: Option<String>,
    pub warm: bool,
    pub keep_alive: Option<serde_json::Value>,
    pub tags_timeout: Duration,
    pub warmup_timeout: Duration,
    /// Hit `/api/ps` and report any drift between the loaded runner's
    /// `context_length` and the `num_ctx` Harn would request.
    pub observe_loaded: bool,
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
            observe_loaded: false,
        }
    }
}

/// Public wrapper around the internal keep-alive parser, used by callers
/// (CLI flags, host bridges) that want the same normalization Harn applies
/// to environment overrides.
pub fn normalize_ollama_keep_alive(raw: &str) -> Option<serde_json::Value> {
    parse_keep_alive_str(raw)
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OllamaRuntimeSettings {
    pub num_ctx: u64,
    pub keep_alive: Value,
}

impl OllamaRuntimeSettings {
    pub fn from_env() -> Self {
        Self::from_env_and_overrides(None)
    }

    pub fn from_env_and_overrides(overrides: Option<&Value>) -> Self {
        Self::from_env_overrides_and_model(overrides, None)
    }

    pub fn from_env_overrides_and_model(overrides: Option<&Value>, model: Option<&str>) -> Self {
        Self {
            num_ctx: num_ctx_from_overrides(overrides)
                .or_else(num_ctx_from_env)
                .or_else(|| num_ctx_from_model_catalog(model))
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
    let settings = OllamaRuntimeSettings::from_env_overrides_and_model(None, Some(model));
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
            .or_else(|| num_ctx_from_model_catalog(body.get("model").and_then(Value::as_str)))
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

fn num_ctx_from_model_catalog(model: Option<&str>) -> Option<u64> {
    let model = model?.trim();
    if model.is_empty() {
        return None;
    }
    let entry = crate::llm_config::model_catalog_entry(model)?;
    entry
        .runtime_context_window
        .filter(|window| *window > 0)
        .or_else(|| (entry.context_window > 0).then_some(entry.context_window))
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
    let mut result = OllamaReadinessResult::probing(base_url.clone(), options.model.clone());

    let tags_url = match ollama_endpoint_url(&base_url, "/api/tags") {
        Ok(url) => url,
        Err(message) => return result.fail("invalid_url", message),
    };
    result.tags_url = tags_url.clone();

    let client = crate::llm::shared_utility_client();
    let response = match client
        .get(tags_url.clone())
        .timeout(options.tags_timeout)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return result.fail(
                "daemon_down",
                format!("Ollama not reachable at {tags_url}: {error}"),
            );
        }
    };

    let status = response.status();
    result.http_status = Some(status.as_u16());
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return result.fail(
            "bad_status",
            format!(
                "Ollama returned HTTP {} from /api/tags: {body}",
                status.as_u16()
            ),
        );
    }

    let body: Value = match response.json().await {
        Ok(value) => value,
        Err(error) => {
            return result.fail(
                "invalid_response",
                format!("Could not parse Ollama model list: {error}"),
            );
        }
    };

    let Some(models) = parse_ollama_model_names(&body) else {
        return result.fail(
            "invalid_response",
            "Could not parse Ollama model list: missing models[].name".to_string(),
        );
    };
    result.available_models = models.clone();

    let Some(matched) = find_ollama_model_match(&models, &options.model) else {
        let available = if models.is_empty() {
            "(none)".to_string()
        } else {
            models.join(", ")
        };
        return result.fail(
            "model_missing",
            format!(
                "Ollama model '{}' not found. Available: {available}",
                options.model
            ),
        );
    };
    result.matched_model = Some(matched.clone());

    let settings = OllamaRuntimeSettings::from_env_overrides_and_model(None, Some(&matched));
    let keep_alive = options
        .keep_alive
        .clone()
        .unwrap_or_else(|| settings.keep_alive.clone());
    result.expected = Some(OllamaExpectedRequest {
        num_ctx: settings.num_ctx,
        keep_alive: keep_alive.clone(),
    });
    result.keep_alive = Some(keep_alive.clone());

    result.message = format!("Ollama is reachable and model '{matched}' is available");
    if options.warm {
        let warm = ollama_warmup(
            &base_url,
            &matched,
            Some(keep_alive),
            options.warmup_timeout,
        )
        .await;
        if !warm.valid {
            result.valid = false;
            result.status = "warmup_failed".to_string();
            result.message = warm.message.clone();
        } else {
            result.message = format!("{}; {}", result.message, warm.message);
        }
        result.warmup = Some(warm);
    }

    if options.observe_loaded {
        match fetch_ollama_loaded_runners(&base_url, options.tags_timeout).await {
            Ok(runners) => {
                if let Some(runner) = match_loaded_runner(runners, &matched) {
                    if let Some(actual) = runner.context_length {
                        if actual != settings.num_ctx {
                            result.context_drift =
                                Some(describe_context_drift(settings.num_ctx, actual));
                        }
                    }
                    result.loaded_runner = Some(runner);
                }
            }
            Err(error) => {
                // /api/ps is best-effort; surface the failure in the
                // message but don't fail the overall readiness check.
                result.message = format!("{}; /api/ps probe skipped: {error}", result.message);
            }
        }
    }

    result
}

impl OllamaReadinessResult {
    fn probing(base_url: String, model: String) -> Self {
        Self {
            valid: true,
            status: "ok".to_string(),
            message: String::new(),
            base_url,
            tags_url: String::new(),
            model,
            matched_model: None,
            available_models: Vec::new(),
            http_status: None,
            keep_alive: None,
            warmup: None,
            expected: None,
            loaded_runner: None,
            context_drift: None,
        }
    }

    fn fail(mut self, status: &str, message: String) -> Self {
        self.valid = false;
        self.status = status.to_string();
        self.message = message;
        self
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

/// Fetch the list of currently-loaded runners from Ollama's `/api/ps`
/// endpoint. Returns an empty list when no models are loaded; returns an
/// error when the daemon is unreachable or the response is malformed.
pub async fn fetch_ollama_loaded_runners(
    base_url: &str,
    timeout: Duration,
) -> Result<Vec<OllamaLoadedRunner>, String> {
    let url = ollama_endpoint_url(base_url, "/api/ps")?;
    let response = crate::llm::shared_utility_client()
        .get(&url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| format!("Ollama /api/ps not reachable at {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Ollama returned HTTP {} from /api/ps",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("Could not parse Ollama /api/ps response: {error}"))?;
    Ok(parse_ollama_loaded_runners(&body))
}

fn parse_ollama_loaded_runners(value: &Value) -> Vec<OllamaLoadedRunner> {
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?.to_string();
            let model = entry
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| name.clone());
            Some(OllamaLoadedRunner {
                name,
                model,
                context_length: entry.get("context_length").and_then(Value::as_u64),
                size_vram: entry.get("size_vram").and_then(Value::as_u64),
                size: entry.get("size").and_then(Value::as_u64),
                expires_at: entry
                    .get("expires_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn match_loaded_runner(
    runners: Vec<OllamaLoadedRunner>,
    model: &str,
) -> Option<OllamaLoadedRunner> {
    runners
        .into_iter()
        .find(|runner| runner.name == model || runner.model == model)
}

fn describe_context_drift(expected: u64, actual: u64) -> String {
    if actual > expected {
        format!(
            "Loaded runner context_length={actual} exceeds expected num_ctx={expected}. \
             Ollama keeps a runner at the context it was loaded with; run \
             `ollama stop <model>` (or wait for keep_alive to expire) and let Harn \
             re-warm it to apply the smaller context."
        )
    } else {
        format!(
            "Loaded runner context_length={actual} is smaller than expected \
             num_ctx={expected}. The runner was loaded at a smaller context — \
             unload with `ollama stop <model>` and let Harn re-warm to expand."
        )
    }
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

    // Derive the warmup body from runtime settings so num_ctx is loaded
    // into the runner the same way chat/completion requests would. Without
    // it, Ollama loads the model at its Modelfile-declared maximum
    // context, and a subsequent chat request asking for a smaller
    // num_ctx cannot shrink an already-loaded runner — see the
    // "Effective vs. loaded context" section of docs/src/llm/providers.md.
    let settings = OllamaRuntimeSettings::from_env_overrides_and_model(None, Some(model));
    let mut body = settings.warmup_body(model);
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
    use crate::http::framing::{http_content_length_from_header_lines, TEST_HTTP_MAX_BODY_BYTES};
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
    fn runtime_settings_use_catalog_context_after_env_and_overrides() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::remove("HARN_OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        crate::llm_config::clear_user_overrides();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.models.insert(
            "qwen-test".to_string(),
            crate::llm_config::ModelDef {
                name: "Qwen Test".to_string(),
                provider: "ollama".to_string(),
                context_window: 100_000,
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                architecture: None,
                local_memory: None,
                runtime_context_window: None,
                stream_timeout: None,
                capabilities: vec![],
                pricing: None,
                deprecated: false,
                deprecation_note: None,
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: crate::llm_config::ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        let settings = OllamaRuntimeSettings::from_env_overrides_and_model(None, Some("qwen-test"));
        assert_eq!(settings.num_ctx, 100_000);

        let env = ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "65536");
        let settings = OllamaRuntimeSettings::from_env_overrides_and_model(None, Some("qwen-test"));
        assert_eq!(settings.num_ctx, 65_536);
        drop(env);

        let overrides = serde_json::json!({"num_ctx": 8192});
        let settings = OllamaRuntimeSettings::from_env_overrides_and_model(
            Some(&overrides),
            Some("qwen-test"),
        );
        assert_eq!(settings.num_ctx, 8_192);

        crate::llm_config::clear_user_overrides();
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
    fn apply_runtime_settings_uses_catalog_context_when_body_has_model() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::remove("HARN_OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        crate::llm_config::clear_user_overrides();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.models.insert(
            "qwen-test".to_string(),
            crate::llm_config::ModelDef {
                name: "Qwen Test".to_string(),
                provider: "ollama".to_string(),
                context_window: 100_000,
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                architecture: None,
                local_memory: None,
                runtime_context_window: Some(32_768),
                stream_timeout: None,
                capabilities: vec![],
                pricing: None,
                deprecated: false,
                deprecation_note: None,
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: crate::llm_config::ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        let mut body = serde_json::json!({
            "model": "qwen-test",
            "options": {"temperature": 0.1}
        });
        apply_ollama_runtime_settings(&mut body, None);
        assert_eq!(body["options"]["num_ctx"], serde_json::json!(32768));
        assert_eq!(body["options"]["temperature"], serde_json::json!(0.1));

        crate::llm_config::clear_user_overrides();
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

    fn readiness_options(model: &str, base_url: String) -> OllamaReadinessOptions {
        OllamaReadinessOptions {
            model: model.to_string(),
            base_url: Some(base_url),
            warm: false,
            keep_alive: None,
            tags_timeout: Duration::from_secs(2),
            warmup_timeout: Duration::from_secs(2),
            observe_loaded: false,
        }
    }

    #[test]
    fn ollama_readiness_verifies_model_and_warms_matched_tag() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "65536"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
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
                warm: true,
                keep_alive: Some(serde_json::json!(-1)),
                ..readiness_options("qwen3", format!("http://{addr}"))
            }));

        server.join().expect("stub server");
        assert!(result.valid, "result was: {result:?}");
        assert_eq!(result.status, "ok");
        assert_eq!(result.matched_model.as_deref(), Some("qwen3:latest"));
        assert!(result.warmup.as_ref().is_some_and(|warm| warm.valid));
        let expected = result.expected.as_ref().expect("expected request");
        assert_eq!(expected.num_ctx, 65_536);
        assert_eq!(expected.keep_alive, serde_json::json!(-1));

        let requests = captured.lock().expect("captured requests");
        assert!(requests[0].starts_with("GET /api/tags "));
        assert!(requests[1].starts_with("POST /api/generate "));
        let body = requests[1].split("\r\n\r\n").nth(1).unwrap_or("");
        let json: serde_json::Value = serde_json::from_str(body).expect("warmup body");
        assert_eq!(json["model"], "qwen3:latest");
        assert_eq!(json["prompt"], "");
        assert_eq!(json["stream"], false);
        assert_eq!(json["keep_alive"], -1);
        assert_eq!(
            json["options"]["num_ctx"], 65_536,
            "warmup must inject num_ctx so Ollama loads the runner at the requested context — see issue #1600"
        );
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
            .block_on(ollama_readiness(readiness_options(
                "qwen3",
                format!("http://{addr}"),
            )));

        server.join().expect("stub server");
        assert!(!result.valid);
        assert_eq!(result.status, "model_missing");
        assert_eq!(result.available_models, vec!["llama3.2:latest"]);
        assert!(result.message.contains("qwen3"));
    }

    #[test]
    fn ollama_readiness_observes_loaded_runner_and_reports_no_drift() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "32768"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (addr, server) = spawn_stub(
            vec![
                (200, r#"{"models":[{"name":"qwen3:latest"}]}"#),
                (
                    200,
                    r#"{"models":[{"name":"qwen3:latest","model":"qwen3:latest","context_length":32768,"size_vram":1234,"size":4321,"expires_at":"2026-05-13T12:00:00Z"}]}"#,
                ),
            ],
            captured.clone(),
        );

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(ollama_readiness(OllamaReadinessOptions {
                observe_loaded: true,
                ..readiness_options("qwen3", format!("http://{addr}"))
            }));

        server.join().expect("stub server");
        assert!(result.valid, "result was: {result:?}");
        let runner = result.loaded_runner.expect("loaded runner present");
        assert_eq!(runner.context_length, Some(32_768));
        assert_eq!(runner.size_vram, Some(1234));
        assert!(
            result.context_drift.is_none(),
            "no drift expected; got {:?}",
            result.context_drift
        );

        let requests = captured.lock().expect("captured requests");
        assert!(requests[0].starts_with("GET /api/tags "));
        assert!(requests[1].starts_with("GET /api/ps "));
    }

    #[test]
    fn ollama_readiness_flags_context_drift_when_loaded_exceeds_expected() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::set("HARN_OLLAMA_NUM_CTX", "32768"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        let (addr, server) = spawn_stub(
            vec![
                (200, r#"{"models":[{"name":"devstral-small-2:24b"}]}"#),
                (
                    200,
                    r#"{"models":[{"name":"devstral-small-2:24b","model":"devstral-small-2:24b","context_length":262144}]}"#,
                ),
            ],
            Arc::new(Mutex::new(Vec::new())),
        );

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(ollama_readiness(OllamaReadinessOptions {
                observe_loaded: true,
                ..readiness_options("devstral-small-2:24b", format!("http://{addr}"))
            }));

        server.join().expect("stub server");
        assert!(result.valid, "result was: {result:?}");
        let drift = result.context_drift.expect("drift expected");
        assert!(drift.contains("262144"), "drift message: {drift}");
        assert!(drift.contains("32768"), "drift message: {drift}");
        assert!(
            drift.contains("ollama stop"),
            "drift message must teach the user how to recover: {drift}"
        );
    }

    #[test]
    fn ollama_readiness_uses_alias_resolved_runtime_settings() {
        let _guard = env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::remove("HARN_OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        crate::llm_config::clear_user_overrides();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.aliases.insert(
            "devstral-small-2".to_string(),
            crate::llm_config::AliasDef {
                id: "devstral-small-2:24b".to_string(),
                provider: "ollama".to_string(),
                tool_format: None,
            },
        );
        overlay.models.insert(
            "devstral-small-2:24b".to_string(),
            crate::llm_config::ModelDef {
                name: "Devstral Small 2 24B".to_string(),
                provider: "ollama".to_string(),
                context_window: 262_144,
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                architecture: None,
                local_memory: None,
                runtime_context_window: Some(98_304),
                stream_timeout: None,
                capabilities: vec![],
                pricing: None,
                deprecated: false,
                deprecation_note: None,
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: crate::llm_config::ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        let (resolved, _) = crate::llm_config::resolve_model("devstral-small-2");
        assert_eq!(resolved, "devstral-small-2:24b");

        let captured = Arc::new(Mutex::new(Vec::new()));
        let (addr, server) = spawn_stub(
            vec![
                (200, r#"{"models":[{"name":"devstral-small-2:24b"}]}"#),
                (200, r#"{"response":"","done":true}"#),
            ],
            captured.clone(),
        );

        let result = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(ollama_readiness(OllamaReadinessOptions {
                warm: true,
                ..readiness_options(&resolved, format!("http://{addr}"))
            }));

        server.join().expect("stub server");
        crate::llm_config::clear_user_overrides();

        assert!(result.valid, "result was: {result:?}");
        let expected = result.expected.expect("expected request populated");
        assert_eq!(
            expected.num_ctx, 98_304,
            "alias-resolved model must pull runtime_context_window from the catalog"
        );

        let requests = captured.lock().expect("captured requests");
        let warmup_body = requests[1].split("\r\n\r\n").nth(1).unwrap_or("");
        let json: serde_json::Value = serde_json::from_str(warmup_body).expect("warmup body");
        assert_eq!(json["options"]["num_ctx"], 98_304);
    }

    #[test]
    fn fetch_ollama_loaded_runners_parses_optional_fields() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (addr, server) = spawn_stub(
            vec![(
                200,
                r#"{"models":[{"name":"a:latest","model":"a:latest"},{"name":"b:latest","model":"b:latest","context_length":8192,"size_vram":42,"expires_at":"now"}]}"#,
            )],
            captured,
        );

        let runners = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(fetch_ollama_loaded_runners(
                &format!("http://{addr}"),
                Duration::from_secs(2),
            ))
            .expect("ps response parses");
        server.join().expect("stub server");

        assert_eq!(runners.len(), 2);
        assert_eq!(runners[0].name, "a:latest");
        assert!(runners[0].context_length.is_none());
        assert_eq!(runners[1].context_length, Some(8192));
        assert_eq!(runners[1].size_vram, Some(42));
        assert_eq!(runners[1].expires_at.as_deref(), Some("now"));
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
                let content_length = match http_content_length_from_header_lines(
                    headers.lines(),
                    TEST_HTTP_MAX_BODY_BYTES,
                ) {
                    Ok(content_length) => content_length,
                    Err(_) => break,
                };
                let Some(body_end) = header_end
                    .checked_add(4)
                    .and_then(|body_start| body_start.checked_add(content_length))
                else {
                    break;
                };
                if data.len() >= body_end {
                    break;
                }
            }
        }
        String::from_utf8(data).expect("utf8 request")
    }
}
