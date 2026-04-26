//! Ollama-specific runtime settings consumed by chat, completion, and
//! model warmup paths.

use serde_json::Value;

pub const OLLAMA_DEFAULT_NUM_CTX: u64 = 32_768;
pub const OLLAMA_DEFAULT_KEEP_ALIVE: &str = "30m";
pub const HARN_OLLAMA_NUM_CTX_ENV: &str = "HARN_OLLAMA_NUM_CTX";
pub const HARN_OLLAMA_KEEP_ALIVE_ENV: &str = "HARN_OLLAMA_KEEP_ALIVE";
pub const OLLAMA_HOST_ENV: &str = "OLLAMA_HOST";

const OLLAMA_NUM_CTX_ENV_KEYS: [&str; 3] = [
    HARN_OLLAMA_NUM_CTX_ENV,
    "OLLAMA_CONTEXT_LENGTH",
    "OLLAMA_NUM_CTX",
];
const OLLAMA_KEEP_ALIVE_ENV_KEYS: [&str; 2] = [HARN_OLLAMA_KEEP_ALIVE_ENV, "OLLAMA_KEEP_ALIVE"];
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::env_lock;

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
}
