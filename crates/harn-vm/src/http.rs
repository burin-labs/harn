use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

// Mock HTTP framework (thread-local, mirrors the mock LLM pattern).

#[derive(Clone)]
struct MockResponse {
    status: i64,
    body: String,
    headers: BTreeMap<String, VmValue>,
}

struct HttpMock {
    method: String,
    url_pattern: String,
    responses: Vec<MockResponse>,
    next_response: usize,
}

#[derive(Clone)]
struct HttpMockCall {
    method: String,
    url: String,
    body: Option<String>,
}

#[derive(Clone)]
struct RetryConfig {
    max: u32,
    backoff_ms: u64,
    retryable_statuses: Vec<u16>,
    retryable_methods: Vec<String>,
    respect_retry_after: bool,
}

#[derive(Clone)]
struct HttpRequestConfig {
    timeout_ms: u64,
    retry: RetryConfig,
    follow_redirects: bool,
    max_redirects: usize,
}

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_BACKOFF_MS: u64 = 1_000;
const MAX_RETRY_DELAY_MS: u64 = 60_000;
const DEFAULT_RETRYABLE_STATUSES: [u16; 6] = [408, 429, 500, 502, 503, 504];
const DEFAULT_RETRYABLE_METHODS: [&str; 5] = ["GET", "HEAD", "PUT", "DELETE", "OPTIONS"];

thread_local! {
    static HTTP_MOCKS: RefCell<Vec<HttpMock>> = const { RefCell::new(Vec::new()) };
    static HTTP_MOCK_CALLS: RefCell<Vec<HttpMockCall>> = const { RefCell::new(Vec::new()) };
}

/// Reset thread-local HTTP mock state. Call between test runs.
pub fn reset_http_state() {
    HTTP_MOCKS.with(|m| m.borrow_mut().clear());
    HTTP_MOCK_CALLS.with(|c| c.borrow_mut().clear());
}

/// Check if a URL matches a mock pattern (exact or glob with `*`).
fn url_matches(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == url;
    }
    // Multi-glob: split on `*` and match segments in order.
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = url;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if i == parts.len() - 1 {
            if !remaining.ends_with(part) {
                return false;
            }
            remaining = "";
        } else {
            match remaining.find(part) {
                Some(pos) => remaining = &remaining[pos + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Build a standard HTTP response dict with status, headers, body, and ok fields.
fn build_http_response(status: i64, headers: BTreeMap<String, VmValue>, body: String) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("status".to_string(), VmValue::Int(status));
    result.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
    result.insert("body".to_string(), VmValue::String(Rc::from(body)));
    result.insert(
        "ok".to_string(),
        VmValue::Bool((200..300).contains(&(status as u16))),
    );
    VmValue::Dict(Rc::new(result))
}

/// Extract URL, validate it, and pull an options dict from `args`.
/// For methods with a body (POST/PUT/PATCH), the body is at index 1 and
/// options at index 2; for methods without (GET/DELETE), options are at index 1.
async fn http_verb_handler(
    method: &str,
    has_body: bool,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let url = args.first().map(|a| a.display()).unwrap_or_default();
    if url.is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "http_{}: URL is required",
            method.to_ascii_lowercase()
        )))));
    }
    let mut options = if has_body {
        match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        }
    } else {
        match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        }
    };
    if has_body {
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        options.insert("body".to_string(), VmValue::String(Rc::from(body)));
    }
    vm_execute_http_request(method, &url, &options).await
}

fn parse_mock_response_dict(response: &BTreeMap<String, VmValue>) -> MockResponse {
    let status = response
        .get("status")
        .and_then(|v| v.as_int())
        .unwrap_or(200);
    let body = response
        .get("body")
        .map(|v| v.display())
        .unwrap_or_default();
    let headers = response
        .get("headers")
        .and_then(|v| v.as_dict())
        .cloned()
        .unwrap_or_default();
    MockResponse {
        status,
        body,
        headers,
    }
}

fn parse_mock_responses(response: &BTreeMap<String, VmValue>) -> Vec<MockResponse> {
    let scripted = response
        .get("responses")
        .and_then(|value| match value {
            VmValue::List(items) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_dict().map(parse_mock_response_dict))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    if scripted.is_empty() {
        vec![parse_mock_response_dict(response)]
    } else {
        scripted
    }
}

fn consume_http_mock(method: &str, url: &str, body: Option<String>) -> Option<MockResponse> {
    let response = HTTP_MOCKS.with(|mocks| {
        let mut mocks = mocks.borrow_mut();
        for mock in mocks.iter_mut() {
            if (mock.method == "*" || mock.method.eq_ignore_ascii_case(method))
                && url_matches(&mock.url_pattern, url)
            {
                let Some(last_index) = mock.responses.len().checked_sub(1) else {
                    continue;
                };
                let index = mock.next_response.min(last_index);
                let response = mock.responses[index].clone();
                if mock.next_response < last_index {
                    mock.next_response += 1;
                }
                return Some(response);
            }
        }
        None
    })?;

    HTTP_MOCK_CALLS.with(|calls| {
        calls.borrow_mut().push(HttpMockCall {
            method: method.to_string(),
            url: url.to_string(),
            body,
        });
    });

    Some(response)
}

/// Register HTTP builtins on a VM.
pub fn register_http_builtins(vm: &mut Vm) {
    vm.register_async_builtin("http_get", |args| async move {
        http_verb_handler("GET", false, args).await
    });
    vm.register_async_builtin("http_post", |args| async move {
        http_verb_handler("POST", true, args).await
    });
    vm.register_async_builtin("http_put", |args| async move {
        http_verb_handler("PUT", true, args).await
    });
    vm.register_async_builtin("http_patch", |args| async move {
        http_verb_handler("PATCH", true, args).await
    });
    vm.register_async_builtin("http_delete", |args| async move {
        http_verb_handler("DELETE", false, args).await
    });

    // --- Mock HTTP builtins ---

    // http_mock(method, url_pattern, response) -> nil
    vm.register_builtin("http_mock", |args, _out| {
        let method = args.first().map(|a| a.display()).unwrap_or_default();
        let url_pattern = args.get(1).map(|a| a.display()).unwrap_or_default();
        let response = args
            .get(2)
            .and_then(|a| a.as_dict())
            .cloned()
            .unwrap_or_default();
        let responses = parse_mock_responses(&response);

        HTTP_MOCKS.with(|mocks| {
            mocks.borrow_mut().push(HttpMock {
                method,
                url_pattern,
                responses,
                next_response: 0,
            });
        });
        Ok(VmValue::Nil)
    });

    // http_mock_clear() -> nil
    vm.register_builtin("http_mock_clear", |_args, _out| {
        HTTP_MOCKS.with(|mocks| mocks.borrow_mut().clear());
        HTTP_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
        Ok(VmValue::Nil)
    });

    // http_mock_calls() -> list of {method, url, body}
    vm.register_builtin("http_mock_calls", |_args, _out| {
        let calls = HTTP_MOCK_CALLS.with(|calls| calls.borrow().clone());
        let result: Vec<VmValue> = calls
            .iter()
            .map(|c| {
                let mut dict = BTreeMap::new();
                dict.insert(
                    "method".to_string(),
                    VmValue::String(Rc::from(c.method.as_str())),
                );
                dict.insert("url".to_string(), VmValue::String(Rc::from(c.url.as_str())));
                dict.insert(
                    "body".to_string(),
                    match &c.body {
                        Some(b) => VmValue::String(Rc::from(b.as_str())),
                        None => VmValue::Nil,
                    },
                );
                VmValue::Dict(Rc::new(dict))
            })
            .collect();
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_async_builtin("http_request", |args| async move {
        let method = args
            .first()
            .map(|a| a.display())
            .unwrap_or_default()
            .to_uppercase();
        if method.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_request: method is required",
            ))));
        }
        let url = args.get(1).map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_request: URL is required",
            ))));
        }
        let options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request(&method, &url, &options).await
    });
}

fn vm_get_int_option(options: &BTreeMap<String, VmValue>, key: &str, default: i64) -> i64 {
    options.get(key).and_then(|v| v.as_int()).unwrap_or(default)
}

fn vm_get_bool_option(options: &BTreeMap<String, VmValue>, key: &str, default: bool) -> bool {
    match options.get(key) {
        Some(VmValue::Bool(b)) => *b,
        _ => default,
    }
}

fn vm_get_int_option_prefer(
    options: &BTreeMap<String, VmValue>,
    canonical: &str,
    alias: &str,
    default: i64,
) -> i64 {
    options
        .get(canonical)
        .and_then(|value| value.as_int())
        .or_else(|| options.get(alias).and_then(|value| value.as_int()))
        .unwrap_or(default)
}

fn parse_retry_statuses(options: &BTreeMap<String, VmValue>) -> Vec<u16> {
    match options.get("retry_on") {
        Some(VmValue::List(values)) => {
            let statuses: Vec<u16> = values
                .iter()
                .filter_map(|value| value.as_int())
                .filter(|status| (0..=u16::MAX as i64).contains(status))
                .map(|status| status as u16)
                .collect();
            if statuses.is_empty() {
                DEFAULT_RETRYABLE_STATUSES.to_vec()
            } else {
                statuses
            }
        }
        _ => DEFAULT_RETRYABLE_STATUSES.to_vec(),
    }
}

fn parse_retry_methods(options: &BTreeMap<String, VmValue>) -> Vec<String> {
    match options.get("retry_methods") {
        Some(VmValue::List(values)) => {
            let methods: Vec<String> = values
                .iter()
                .map(|value| value.display().trim().to_ascii_uppercase())
                .filter(|value| !value.is_empty())
                .collect();
            if methods.is_empty() {
                DEFAULT_RETRYABLE_METHODS
                    .iter()
                    .map(|method| (*method).to_string())
                    .collect()
            } else {
                methods
            }
        }
        _ => DEFAULT_RETRYABLE_METHODS
            .iter()
            .map(|method| (*method).to_string())
            .collect(),
    }
}

fn parse_http_options(options: &BTreeMap<String, VmValue>) -> HttpRequestConfig {
    let timeout_ms = vm_get_int_option_prefer(
        options,
        "timeout_ms",
        "timeout",
        DEFAULT_TIMEOUT_MS as i64,
    )
    .max(0) as u64;
    let retry_options = options.get("retry").and_then(|value| value.as_dict());
    let retry_max = retry_options
        .and_then(|retry| retry.get("max"))
        .and_then(|value| value.as_int())
        .unwrap_or_else(|| vm_get_int_option(options, "retries", 0))
        .max(0) as u32;
    let retry_backoff_ms = retry_options
        .and_then(|retry| retry.get("backoff_ms"))
        .and_then(|value| value.as_int())
        .unwrap_or_else(|| vm_get_int_option(options, "backoff", DEFAULT_BACKOFF_MS as i64))
        .max(0) as u64;
    let respect_retry_after = vm_get_bool_option(options, "respect_retry_after", true);
    let follow_redirects = vm_get_bool_option(options, "follow_redirects", true);
    let max_redirects = vm_get_int_option(options, "max_redirects", 10).max(0) as usize;

    HttpRequestConfig {
        timeout_ms,
        retry: RetryConfig {
            max: retry_max,
            backoff_ms: retry_backoff_ms,
            retryable_statuses: parse_retry_statuses(options),
            retryable_methods: parse_retry_methods(options),
            respect_retry_after,
        },
        follow_redirects,
        max_redirects,
    }
}

fn method_is_retryable(retry: &RetryConfig, method: &reqwest::Method) -> bool {
    retry
        .retryable_methods
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(method.as_str()))
}

fn should_retry_response(
    config: &HttpRequestConfig,
    method: &reqwest::Method,
    status: u16,
    attempt: u32,
) -> bool {
    attempt < config.retry.max
        && method_is_retryable(&config.retry, method)
        && config.retry.retryable_statuses.contains(&status)
}

fn should_retry_transport(
    config: &HttpRequestConfig,
    method: &reqwest::Method,
    error: &reqwest::Error,
    attempt: u32,
) -> bool {
    attempt < config.retry.max
        && method_is_retryable(&config.retry, method)
        && (error.is_timeout() || error.is_connect())
}

fn parse_retry_after_value(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(secs) = value.parse::<f64>() {
        if !secs.is_finite() || secs < 0.0 {
            return Some(Duration::from_millis(0));
        }
        let millis = (secs * 1_000.0) as u64;
        return Some(Duration::from_millis(millis.min(MAX_RETRY_DELAY_MS)));
    }

    if let Ok(target) = httpdate::parse_http_date(value) {
        let millis = target
            .duration_since(SystemTime::now())
            .map(|delta| delta.as_millis() as u64)
            .unwrap_or(0);
        return Some(Duration::from_millis(millis.min(MAX_RETRY_DELAY_MS)));
    }

    None
}

fn parse_retry_after_header(value: &reqwest::header::HeaderValue) -> Option<Duration> {
    value.to_str().ok().and_then(parse_retry_after_value)
}

fn mock_retry_after(status: u16, headers: &BTreeMap<String, VmValue>) -> Option<Duration> {
    if !(status == 429 || status == 503) {
        return None;
    }

    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| parse_retry_after_value(&value.display()))
}

fn response_retry_after(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    respect_retry_after: bool,
) -> Option<Duration> {
    if !respect_retry_after || !(status == 429 || status == 503) {
        return None;
    }
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(parse_retry_after_header)
}

fn compute_retry_delay(attempt: u32, base_ms: u64, retry_after: Option<Duration>) -> Duration {
    use rand::RngExt;

    let base_delay = base_ms.saturating_mul(1u64 << attempt.min(30));
    let jitter: f64 = rand::rng().random_range(0.75..=1.25);
    let exponential_ms = ((base_delay as f64 * jitter) as u64).min(MAX_RETRY_DELAY_MS);
    let retry_after_ms = retry_after
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
        .min(MAX_RETRY_DELAY_MS);
    Duration::from_millis(exponential_ms.max(retry_after_ms))
}

async fn vm_execute_http_request(
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let config = parse_http_options(options);

    let redirect_policy = if config.follow_redirects {
        reqwest::redirect::Policy::limited(config.max_redirects)
    } else {
        reqwest::redirect::Policy::none()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(config.timeout_ms))
        .redirect(redirect_policy)
        .build()
        .map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "http: failed to build client: {e}"
            ))))
        })?;

    let req_method = method.parse::<reqwest::Method>().map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "http: invalid method '{method}': {e}"
        ))))
    })?;

    let mut header_map = reqwest::header::HeaderMap::new();

    if let Some(auth_val) = options.get("auth") {
        match auth_val {
            VmValue::String(s) => {
                let hv = reqwest::header::HeaderValue::from_str(s).map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: invalid auth header value: {e}"
                    ))))
                })?;
                header_map.insert(reqwest::header::AUTHORIZATION, hv);
            }
            VmValue::Dict(d) => {
                if let Some(bearer) = d.get("bearer") {
                    let token = bearer.display();
                    let hv = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                        .map_err(|e| {
                            VmError::Thrown(VmValue::String(Rc::from(format!(
                                "http: invalid bearer token: {e}"
                            ))))
                        })?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                } else if let Some(VmValue::Dict(basic)) = d.get("basic") {
                    let user = basic.get("user").map(|v| v.display()).unwrap_or_default();
                    let password = basic
                        .get("password")
                        .map(|v| v.display())
                        .unwrap_or_default();
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{user}:{password}"));
                    let hv = reqwest::header::HeaderValue::from_str(&format!("Basic {encoded}"))
                        .map_err(|e| {
                            VmError::Thrown(VmValue::String(Rc::from(format!(
                                "http: invalid basic auth: {e}"
                            ))))
                        })?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                }
            }
            _ => {}
        }
    }

    if let Some(VmValue::Dict(hdrs)) = options.get("headers") {
        for (k, v) in hdrs.iter() {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: invalid header name '{k}': {e}"
                ))))
            })?;
            let val = reqwest::header::HeaderValue::from_str(&v.display()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: invalid header value for '{k}': {e}"
                ))))
            })?;
            header_map.insert(name, val);
        }
    }

    let body_str = options.get("body").map(|v| v.display());

    for attempt in 0..=config.retry.max {
        if let Some(mock_response) = consume_http_mock(method, url, body_str.clone()) {
            let status = mock_response.status.clamp(0, u16::MAX as i64) as u16;
            if should_retry_response(&config, &req_method, status, attempt) {
                let retry_after = if config.retry.respect_retry_after {
                    mock_retry_after(status, &mock_response.headers)
                } else {
                    None
                };
                tokio::time::sleep(compute_retry_delay(
                    attempt,
                    config.retry.backoff_ms,
                    retry_after,
                ))
                .await;
                continue;
            }

            return Ok(build_http_response(
                mock_response.status,
                mock_response.headers,
                mock_response.body,
            ));
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "http: URL must start with http:// or https://, got '{url}'"
            )))));
        }

        let mut req = client.request(req_method.clone(), url);
        req = req.headers(header_map.clone());
        if let Some(ref b) = body_str {
            req = req.body(b.clone());
        }

        match req.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                if should_retry_response(&config, &req_method, status, attempt) {
                    let retry_after = response_retry_after(
                        status,
                        response.headers(),
                        config.retry.respect_retry_after,
                    );
                    tokio::time::sleep(compute_retry_delay(
                        attempt,
                        config.retry.backoff_ms,
                        retry_after,
                    ))
                    .await;
                    continue;
                }

                let mut resp_headers = BTreeMap::new();
                for (name, value) in response.headers() {
                    if let Ok(v) = value.to_str() {
                        resp_headers
                            .insert(name.as_str().to_string(), VmValue::String(Rc::from(v)));
                    }
                }

                let body_text = response.text().await.map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: failed to read response body: {e}"
                    ))))
                })?;
                return Ok(build_http_response(status as i64, resp_headers, body_text));
            }
            Err(e) => {
                if should_retry_transport(&config, &req_method, &e, attempt) {
                    tokio::time::sleep(compute_retry_delay(attempt, config.retry.backoff_ms, None))
                        .await;
                    continue;
                }
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: request failed: {e}"
                )))));
            }
        }
    }

    Err(VmError::Thrown(VmValue::String(Rc::from(
        "http: request failed",
    ))))
}

#[cfg(test)]
mod tests {
    use super::{compute_retry_delay, parse_retry_after_value};
    use std::time::{Duration, SystemTime};

    #[test]
    fn parses_retry_after_delta_seconds() {
        assert_eq!(parse_retry_after_value("5"), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parses_retry_after_http_date() {
        let header = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(2));
        let parsed = parse_retry_after_value(&header).expect("http-date should parse");
        assert!(parsed <= Duration::from_secs(2));
        assert!(parsed <= Duration::from_secs(60));
    }

    #[test]
    fn malformed_retry_after_returns_none() {
        assert_eq!(parse_retry_after_value("soon-ish"), None);
    }

    #[test]
    fn retry_delay_honors_retry_after_floor() {
        let delay = compute_retry_delay(0, 1, Some(Duration::from_millis(250)));
        assert!(delay >= Duration::from_millis(250));
        assert!(delay <= Duration::from_secs(60));
    }
}
