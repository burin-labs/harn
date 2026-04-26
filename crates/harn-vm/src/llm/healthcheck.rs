use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::llm_config::{self, AuthEnv, HealthcheckDef, ProviderDef};

use super::api::apply_auth_headers;

const DEFAULT_HEALTHCHECK_TIMEOUT_SECS: u64 = 5;
const BODY_SNIPPET_LIMIT: usize = 1000;

#[derive(Debug, Clone, Default)]
pub struct ProviderHealthcheckOptions {
    /// Candidate API key to validate. When unset, Harn resolves credentials
    /// from the provider's configured environment variables.
    pub api_key: Option<String>,
    /// Optional client override for hosts that need custom transport policy.
    pub client: Option<reqwest::Client>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderHealthcheckResult {
    pub provider: String,
    pub valid: bool,
    pub message: String,
    pub metadata: BTreeMap<String, JsonValue>,
}

impl ProviderHealthcheckResult {
    fn new(
        provider: impl Into<String>,
        valid: bool,
        message: impl Into<String>,
        metadata: BTreeMap<String, JsonValue>,
    ) -> Self {
        Self {
            provider: provider.into(),
            valid,
            message: message.into(),
            metadata,
        }
    }
}

pub async fn run_provider_healthcheck(provider: &str) -> ProviderHealthcheckResult {
    run_provider_healthcheck_with_options(provider, ProviderHealthcheckOptions::default()).await
}

pub async fn run_provider_healthcheck_with_options(
    provider: &str,
    options: ProviderHealthcheckOptions,
) -> ProviderHealthcheckResult {
    let provider = if provider.trim().is_empty() {
        "anthropic"
    } else {
        provider.trim()
    };

    let Some(def) = llm_config::provider_config(provider) else {
        let mut metadata = base_metadata("unknown_provider");
        metadata.insert("provider".to_string(), json!(provider));
        return ProviderHealthcheckResult::new(
            provider,
            false,
            format!("Unknown provider: {provider}"),
            metadata,
        );
    };

    let Some(healthcheck) = def.healthcheck.as_ref() else {
        let mut metadata = base_metadata("no_healthcheck");
        metadata.insert("provider".to_string(), json!(provider));
        return ProviderHealthcheckResult::new(
            provider,
            false,
            format!("No healthcheck configured for {provider}"),
            metadata,
        );
    };

    let auth = resolve_healthcheck_auth(&def, options.api_key);
    if auth.requires_auth && auth.api_key.is_none() {
        let mut metadata = base_metadata("missing_credentials");
        metadata.insert("provider".to_string(), json!(provider));
        metadata.insert("auth_env".to_string(), json!(auth.candidates));
        return ProviderHealthcheckResult::new(
            provider,
            false,
            format!(
                "Missing credentials for {provider}: set {} or pass an api_key",
                auth.candidates.join(", ")
            ),
            metadata,
        );
    }

    let url = build_healthcheck_url(&def, healthcheck);
    let method = Method::from_bytes(healthcheck.method.as_bytes()).unwrap_or(Method::GET);
    let client = match options.client {
        Some(client) => client,
        None => match reqwest::Client::builder()
            .timeout(Duration::from_secs(DEFAULT_HEALTHCHECK_TIMEOUT_SECS))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                let mut metadata = base_metadata("client_build_failed");
                metadata.insert("provider".to_string(), json!(provider));
                return ProviderHealthcheckResult::new(
                    provider,
                    false,
                    format!("{provider} healthcheck failed: {error}"),
                    metadata,
                );
            }
        },
    };

    let mut request = client.request(method.clone(), &url);
    if let Some(api_key) = auth.api_key.as_deref() {
        request = apply_auth_headers(request, api_key, Some(&def));
    }
    for (name, value) in &def.extra_headers {
        request = request.header(name, value);
    }
    if let Some(body) = &healthcheck.body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let status_code = status.as_u16();
            let valid = status.is_success();
            let body_text = response.text().await.unwrap_or_default();
            let mut metadata = base_metadata(if valid { "ok" } else { "http_status" });
            metadata.insert("provider".to_string(), json!(provider));
            metadata.insert("status".to_string(), json!(status_code));
            metadata.insert("url".to_string(), json!(url));
            metadata.insert("method".to_string(), json!(method.as_str()));
            if !valid && !body_text.is_empty() {
                metadata.insert("body".to_string(), json!(body_snippet(&body_text)));
            }

            let message = if valid {
                format!("{provider} is reachable (HTTP {status_code})")
            } else {
                let suffix = body_snippet(&body_text);
                if suffix.is_empty() {
                    format!("{provider} returned HTTP {status_code}")
                } else {
                    format!("{provider} returned HTTP {status_code}: {suffix}")
                }
            };

            ProviderHealthcheckResult::new(provider, valid, message, metadata)
        }
        Err(error) => {
            let mut metadata = base_metadata("request_failed");
            metadata.insert("provider".to_string(), json!(provider));
            metadata.insert("url".to_string(), json!(url));
            metadata.insert("method".to_string(), json!(method.as_str()));
            ProviderHealthcheckResult::new(
                provider,
                false,
                format!("{provider} healthcheck failed: {error}"),
                metadata,
            )
        }
    }
}

pub fn build_healthcheck_url(def: &ProviderDef, healthcheck: &HealthcheckDef) -> String {
    if let Some(url) = &healthcheck.url {
        return url.clone();
    }

    let base = llm_config::resolve_base_url(def);
    let path = healthcheck.path.as_deref().unwrap_or("");
    if path.starts_with('/') {
        format!("{}{}", base.trim_end_matches('/'), path)
    } else if path.is_empty() {
        base
    } else {
        format!("{}/{}", base.trim_end_matches('/'), path)
    }
}

#[derive(Debug, Clone)]
struct ResolvedHealthcheckAuth {
    requires_auth: bool,
    api_key: Option<String>,
    candidates: Vec<String>,
}

fn resolve_healthcheck_auth(
    def: &ProviderDef,
    api_key_override: Option<String>,
) -> ResolvedHealthcheckAuth {
    let candidates = auth_env_candidates(&def.auth_env);
    if def.auth_style == "none" || matches!(def.auth_env, AuthEnv::None) {
        let api_key = api_key_override.and_then(non_empty);
        return ResolvedHealthcheckAuth {
            requires_auth: api_key.is_some(),
            api_key,
            candidates,
        };
    }

    let api_key = api_key_override
        .and_then(non_empty)
        .or_else(|| resolve_api_key_from_env(&def.auth_env));
    ResolvedHealthcheckAuth {
        requires_auth: true,
        api_key,
        candidates,
    }
}

fn auth_env_candidates(auth_env: &AuthEnv) -> Vec<String> {
    match auth_env {
        AuthEnv::None => Vec::new(),
        AuthEnv::Single(env) => vec![env.clone()],
        AuthEnv::Multiple(envs) => envs.clone(),
    }
}

fn resolve_api_key_from_env(auth_env: &AuthEnv) -> Option<String> {
    match auth_env {
        AuthEnv::None => None,
        AuthEnv::Single(env) => std::env::var(env).ok().and_then(non_empty),
        AuthEnv::Multiple(envs) => envs
            .iter()
            .find_map(|env| std::env::var(env).ok().and_then(non_empty)),
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn base_metadata(reason: &str) -> BTreeMap<String, JsonValue> {
    BTreeMap::from([("reason".to_string(), json!(reason))])
}

fn body_snippet(body: &str) -> String {
    let mut snippet = String::new();
    for ch in body.chars().take(BODY_SNIPPET_LIMIT) {
        snippet.push(ch);
    }
    snippet
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    fn provider_with_healthcheck(base_url: String, healthcheck: HealthcheckDef) -> ProviderDef {
        ProviderDef {
            base_url,
            auth_style: "bearer".to_string(),
            auth_env: AuthEnv::Single("HARN_TEST_PROVIDER_KEY".to_string()),
            extra_headers: BTreeMap::from([("x-extra".to_string(), "extra-value".to_string())]),
            chat_endpoint: "/chat/completions".to_string(),
            healthcheck: Some(healthcheck),
            ..Default::default()
        }
    }

    fn install_provider(name: &str, provider: ProviderDef) {
        let mut config = llm_config::ProvidersConfig::default();
        config.providers.insert(name.to_string(), provider);
        llm_config::set_user_overrides(Some(config));
    }

    fn spawn_healthcheck_stub(
        status: u16,
        body: &'static str,
        captured: Arc<Mutex<Option<String>>>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind healthcheck stub");
        let addr = listener.local_addr().expect("stub addr");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");

        // Use a generous deadline so the stub doesn't trip when nextest fans
        // out across the workspace and starves this thread of CPU. The
        // healthcheck client itself completes in milliseconds against the
        // loopback stub once it gets scheduled — the deadline is just an
        // upper bound to keep a stuck test from hanging forever.
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            panic!("healthcheck stub: no client within 30s");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("healthcheck stub accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(30)))
                .ok();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(30)))
                .ok();

            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).expect("read request");
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                let request = String::from_utf8_lossy(&bytes);
                if request_complete(&request) {
                    break;
                }
            }
            *captured.lock().expect("capture request") =
                Some(String::from_utf8_lossy(&bytes).to_string());

            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        (format!("http://{addr}"), handle)
    }

    fn request_complete(request: &str) -> bool {
        let Some((headers, body)) = request.split_once("\r\n\r\n") else {
            return false;
        };
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .or_else(|| {
                headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        body.len() >= content_length
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn sends_configured_probe_request_with_candidate_key() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let captured = Arc::new(Mutex::new(None));
        let (base_url, server) = spawn_healthcheck_stub(200, r#"{"ok":true}"#, captured.clone());
        install_provider(
            "acme",
            provider_with_healthcheck(
                base_url.clone(),
                HealthcheckDef {
                    method: "POST".to_string(),
                    path: Some("probe".to_string()),
                    url: None,
                    body: Some(r#"{"ping":true}"#.to_string()),
                },
            ),
        );

        let result = run_provider_healthcheck_with_options(
            "acme",
            ProviderHealthcheckOptions {
                api_key: Some("candidate-key".to_string()),
                client: None,
            },
        )
        .await;
        server.join().expect("stub server");
        llm_config::clear_user_overrides();

        assert!(result.valid);
        assert_eq!(result.provider, "acme");
        assert_eq!(result.metadata["status"], json!(200));
        assert_eq!(result.metadata["method"], json!("POST"));
        assert_eq!(result.metadata["url"], json!(format!("{base_url}/probe")));

        let request = captured
            .lock()
            .expect("captured request")
            .clone()
            .expect("request");
        assert!(request.starts_with("POST /probe HTTP/1.1\r\n"));
        assert!(request.contains("authorization: Bearer candidate-key\r\n"));
        assert!(request.contains("x-extra: extra-value\r\n"));
        assert!(request.ends_with(r#"{"ping":true}"#));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn reports_missing_credentials_without_network() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        unsafe {
            std::env::remove_var("HARN_TEST_PROVIDER_KEY");
        }
        install_provider(
            "acme-missing-key",
            provider_with_healthcheck(
                "http://127.0.0.1:9".to_string(),
                HealthcheckDef {
                    method: "GET".to_string(),
                    path: Some("/models".to_string()),
                    url: None,
                    body: None,
                },
            ),
        );

        let result = run_provider_healthcheck("acme-missing-key").await;
        llm_config::clear_user_overrides();

        assert!(!result.valid);
        assert_eq!(result.metadata["reason"], json!("missing_credentials"));
        assert_eq!(
            result.metadata["auth_env"],
            json!(["HARN_TEST_PROVIDER_KEY"])
        );
        assert!(result.message.contains("Missing credentials"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn returns_stable_failure_shape_for_http_errors() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let captured = Arc::new(Mutex::new(None));
        let (base_url, server) = spawn_healthcheck_stub(401, r#"{"error":"bad key"}"#, captured);
        install_provider(
            "acme-auth",
            provider_with_healthcheck(
                base_url,
                HealthcheckDef {
                    method: "GET".to_string(),
                    path: Some("/models".to_string()),
                    url: None,
                    body: None,
                },
            ),
        );

        let result = run_provider_healthcheck_with_options(
            "acme-auth",
            ProviderHealthcheckOptions {
                api_key: Some("bad-key".to_string()),
                client: None,
            },
        )
        .await;
        server.join().expect("stub server");
        llm_config::clear_user_overrides();

        assert!(!result.valid);
        assert_eq!(result.provider, "acme-auth");
        assert_eq!(result.metadata["reason"], json!("http_status"));
        assert_eq!(result.metadata["status"], json!(401));
        assert_eq!(result.metadata["body"], json!(r#"{"error":"bad key"}"#));
    }

    #[test]
    fn default_external_provider_catalog_has_healthchecks() {
        for provider in [
            "openrouter",
            "anthropic",
            "openai",
            "huggingface",
            "together",
        ] {
            let config = llm_config::provider_config(provider)
                .unwrap_or_else(|| panic!("missing provider {provider}"));
            let healthcheck = config
                .healthcheck
                .as_ref()
                .unwrap_or_else(|| panic!("missing healthcheck for {provider}"));
            assert!(!healthcheck.method.is_empty());
            assert!(healthcheck.path.is_some() || healthcheck.url.is_some());
        }
    }
}
