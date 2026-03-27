use std::collections::BTreeMap;
use std::time::Duration;

use harn_runtime::{Interpreter, RuntimeError, Value};

/// Register all HTTP builtins on an interpreter.
pub fn register_http_builtins(interp: &mut Interpreter) {
    // http_request(method, url, options?) -> {status, headers, body, ok}
    interp.register_async_builtin("http_request", |args| async move {
        let method = args
            .first()
            .map(|a| a.as_string())
            .unwrap_or_default()
            .to_uppercase();
        if method.is_empty() {
            return Err(RuntimeError::thrown("http_request: method is required"));
        }
        let url = args.get(1).map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_request: URL is required"));
        }
        let options = match args.get(2) {
            Some(Value::Dict(d)) => d.clone(),
            _ => BTreeMap::new(),
        };
        execute_request(&method, &url, &options).await
    });

    // http_get(url, options?) -> {status, headers, body, ok}
    interp.register_async_builtin("http_get", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_get: URL is required"));
        }
        let options = match args.get(1) {
            Some(Value::Dict(d)) => d.clone(),
            _ => BTreeMap::new(),
        };
        execute_request("GET", &url, &options).await
    });

    // http_post(url, body, options?) -> {status, headers, body, ok}
    interp.register_async_builtin("http_post", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_post: URL is required"));
        }
        let body = args.get(1).map(|a| a.as_string()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(Value::Dict(d)) => d.clone(),
            _ => BTreeMap::new(),
        };
        options
            .entry("body".to_string())
            .or_insert(Value::String(body));
        execute_request("POST", &url, &options).await
    });

    // http_put(url, body, options?) -> {status, headers, body, ok}
    interp.register_async_builtin("http_put", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_put: URL is required"));
        }
        let body = args.get(1).map(|a| a.as_string()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(Value::Dict(d)) => d.clone(),
            _ => BTreeMap::new(),
        };
        options
            .entry("body".to_string())
            .or_insert(Value::String(body));
        execute_request("PUT", &url, &options).await
    });

    // http_patch(url, body, options?) -> {status, headers, body, ok}
    interp.register_async_builtin("http_patch", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_patch: URL is required"));
        }
        let body = args.get(1).map(|a| a.as_string()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(Value::Dict(d)) => d.clone(),
            _ => BTreeMap::new(),
        };
        options
            .entry("body".to_string())
            .or_insert(Value::String(body));
        execute_request("PATCH", &url, &options).await
    });

    // http_delete(url, options?) -> {status, headers, body, ok}
    interp.register_async_builtin("http_delete", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_delete: URL is required"));
        }
        let options = match args.get(1) {
            Some(Value::Dict(d)) => d.clone(),
            _ => BTreeMap::new(),
        };
        execute_request("DELETE", &url, &options).await
    });
}

/// Extract an integer option from the options dict, returning a default if missing.
fn get_int_option(options: &BTreeMap<String, Value>, key: &str, default: i64) -> i64 {
    options.get(key).and_then(|v| v.as_int()).unwrap_or(default)
}

/// Extract a boolean option from the options dict, returning a default if missing.
fn get_bool_option(options: &BTreeMap<String, Value>, key: &str, default: bool) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        _ => default,
    }
}

/// Determine if a reqwest error is retryable (timeout or connection error).
fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

/// Execute an HTTP request with retry support.
///
/// Options dict supports:
///   timeout (int, ms, default 30000)
///   retries (int, default 0)
///   backoff (int, ms, default 1000)
///   headers (dict of string->string)
///   body (string)
///   auth (string or dict)
///   follow_redirects (bool, default true)
///   max_redirects (int, default 10)
async fn execute_request(
    method: &str,
    url: &str,
    options: &BTreeMap<String, Value>,
) -> Result<Value, RuntimeError> {
    let timeout_ms = get_int_option(options, "timeout", 30_000).max(0) as u64;
    let retries = get_int_option(options, "retries", 0).max(0) as u32;
    let backoff_ms = get_int_option(options, "backoff", 1000).max(0) as u64;
    let follow_redirects = get_bool_option(options, "follow_redirects", true);
    let max_redirects = get_int_option(options, "max_redirects", 10).max(0) as usize;

    // Build the reqwest client once for all retry attempts.
    let redirect_policy = if follow_redirects {
        reqwest::redirect::Policy::limited(max_redirects)
    } else {
        reqwest::redirect::Policy::none()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(redirect_policy)
        .build()
        .map_err(|e| RuntimeError::thrown(format!("http: failed to build client: {e}")))?;

    // Parse the HTTP method.
    let req_method = method
        .parse::<reqwest::Method>()
        .map_err(|e| RuntimeError::thrown(format!("http: invalid method '{method}': {e}")))?;

    // Collect custom headers.
    let mut header_map = reqwest::header::HeaderMap::new();

    // Apply auth option first so custom headers can override.
    if let Some(auth_val) = options.get("auth") {
        match auth_val {
            Value::String(s) => {
                let hv = reqwest::header::HeaderValue::from_str(s).map_err(|e| {
                    RuntimeError::thrown(format!("http: invalid auth header value: {e}"))
                })?;
                header_map.insert(reqwest::header::AUTHORIZATION, hv);
            }
            Value::Dict(d) => {
                if let Some(bearer) = d.get("bearer") {
                    let token = bearer.as_string();
                    let hv = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                        .map_err(|e| {
                            RuntimeError::thrown(format!("http: invalid bearer token: {e}"))
                        })?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                } else if let Some(Value::Dict(basic)) = d.get("basic") {
                    let user = basic.get("user").map(|v| v.as_string()).unwrap_or_default();
                    let password = basic
                        .get("password")
                        .map(|v| v.as_string())
                        .unwrap_or_default();
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{user}:{password}"));
                    let hv = reqwest::header::HeaderValue::from_str(&format!("Basic {encoded}"))
                        .map_err(|e| {
                            RuntimeError::thrown(format!("http: invalid basic auth: {e}"))
                        })?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                }
            }
            _ => {}
        }
    }

    // Apply explicit headers (after auth so they can override).
    if let Some(Value::Dict(hdrs)) = options.get("headers") {
        for (k, v) in hdrs {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                RuntimeError::thrown(format!("http: invalid header name '{k}': {e}"))
            })?;
            let val = reqwest::header::HeaderValue::from_str(&v.as_string()).map_err(|e| {
                RuntimeError::thrown(format!("http: invalid header value for '{k}': {e}"))
            })?;
            header_map.insert(name, val);
        }
    }

    // Extract body.
    let body_str = options.get("body").map(|v| v.as_string());

    // Retry loop.
    let mut last_err: Option<RuntimeError> = None;
    let total_attempts = 1 + retries;

    for attempt in 0..total_attempts {
        if attempt > 0 {
            // Exponential backoff with +/-25% jitter.
            let base_delay = backoff_ms.saturating_mul(1u64 << (attempt - 1).min(30));
            let jitter = jitter_factor();
            let delay_ms = (base_delay as f64 * jitter) as u64;
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let mut req = client.request(req_method.clone(), url);
        req = req.headers(header_map.clone());
        if let Some(ref b) = body_str {
            req = req.body(b.clone());
        }

        match req.send().await {
            Ok(response) => {
                let status = response.status().as_u16() as i64;
                let ok = (200..300).contains(&(status as u16));

                // Collect response headers.
                let mut resp_headers = BTreeMap::new();
                for (name, value) in response.headers() {
                    if let Ok(v) = value.to_str() {
                        resp_headers
                            .insert(name.as_str().to_string(), Value::String(v.to_string()));
                    }
                }

                let body_text = response.text().await.map_err(|e| {
                    RuntimeError::thrown(format!("http: failed to read response body: {e}"))
                })?;

                // If status is 5xx and we have retries left, retry.
                if status >= 500 && attempt + 1 < total_attempts {
                    last_err = Some(RuntimeError::thrown(format!("http: server error {status}")));
                    continue;
                }

                let mut result = BTreeMap::new();
                result.insert("status".to_string(), Value::Int(status));
                result.insert("headers".to_string(), Value::Dict(resp_headers));
                result.insert("body".to_string(), Value::String(body_text));
                result.insert("ok".to_string(), Value::Bool(ok));
                return Ok(Value::Dict(result));
            }
            Err(e) => {
                if is_retryable_error(&e) && attempt + 1 < total_attempts {
                    last_err = Some(RuntimeError::thrown(format!("http: request failed: {e}")));
                    continue;
                }
                return Err(RuntimeError::thrown(format!("http: request failed: {e}")));
            }
        }
    }

    // All retries exhausted — return the last error.
    Err(last_err.unwrap_or_else(|| RuntimeError::thrown("http: request failed")))
}

/// Generate a jitter factor between 0.75 and 1.25.
fn jitter_factor() -> f64 {
    use rand::Rng;
    let jitter: f64 = rand::thread_rng().gen_range(0.75..=1.25);
    jitter
}
