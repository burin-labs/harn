use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

// =============================================================================
// Mock HTTP framework (thread-local, mirrors the mock LLM pattern)
// =============================================================================

struct HttpMock {
    method: String,
    url_pattern: String,
    status: i64,
    body: String,
    headers: BTreeMap<String, VmValue>,
}

#[derive(Clone)]
struct HttpMockCall {
    method: String,
    url: String,
    body: Option<String>,
}

thread_local! {
    static HTTP_MOCKS: RefCell<Vec<HttpMock>> = const { RefCell::new(Vec::new()) };
    static HTTP_MOCK_CALLS: RefCell<Vec<HttpMockCall>> = const { RefCell::new(Vec::new()) };
}

/// Check if a URL matches a mock pattern (exact or glob with `*`).
fn url_matches(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == url;
    }
    // Simple glob: split on `*` and check prefix/suffix containment
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        return url.starts_with(parts[0]) && url.ends_with(parts[1]);
    }
    pattern == url
}

/// Register HTTP builtins on a VM.
pub fn register_http_builtins(vm: &mut Vm) {
    vm.register_async_builtin("http_get", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_get: URL is required",
            ))));
        }
        let options = match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request("GET", &url, &options).await
    });

    vm.register_async_builtin("http_post", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_post: URL is required",
            ))));
        }
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        options.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        vm_execute_http_request("POST", &url, &options).await
    });

    vm.register_async_builtin("http_put", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_put: URL is required",
            ))));
        }
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        options.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        vm_execute_http_request("PUT", &url, &options).await
    });

    vm.register_async_builtin("http_patch", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_patch: URL is required",
            ))));
        }
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        options.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        vm_execute_http_request("PATCH", &url, &options).await
    });

    vm.register_async_builtin("http_delete", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_delete: URL is required",
            ))));
        }
        let options = match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request("DELETE", &url, &options).await
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

        HTTP_MOCKS.with(|mocks| {
            mocks.borrow_mut().push(HttpMock {
                method,
                url_pattern,
                status,
                body,
                headers,
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

// =============================================================================
// HTTP request helpers
// =============================================================================

fn vm_get_int_option(options: &BTreeMap<String, VmValue>, key: &str, default: i64) -> i64 {
    options.get(key).and_then(|v| v.as_int()).unwrap_or(default)
}

fn vm_get_bool_option(options: &BTreeMap<String, VmValue>, key: &str, default: bool) -> bool {
    match options.get(key) {
        Some(VmValue::Bool(b)) => *b,
        _ => default,
    }
}

async fn vm_execute_http_request(
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    use std::time::Duration;

    // Check mock responses first
    let mock_match = HTTP_MOCKS.with(|mocks| {
        let mocks = mocks.borrow();
        for mock in mocks.iter() {
            if (mock.method == "*" || mock.method.eq_ignore_ascii_case(method))
                && url_matches(&mock.url_pattern, url)
            {
                return Some((mock.status, mock.body.clone(), mock.headers.clone()));
            }
        }
        None
    });

    if let Some((status, body, headers)) = mock_match {
        // Record the call
        let body_str = options.get("body").map(|v| v.display());
        HTTP_MOCK_CALLS.with(|calls| {
            calls.borrow_mut().push(HttpMockCall {
                method: method.to_string(),
                url: url.to_string(),
                body: body_str,
            });
        });
        // Return mock response
        let mut result = BTreeMap::new();
        result.insert("status".to_string(), VmValue::Int(status));
        result.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
        result.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        result.insert(
            "ok".to_string(),
            VmValue::Bool((200..300).contains(&(status as u16))),
        );
        return Ok(VmValue::Dict(Rc::new(result)));
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "http: URL must start with http:// or https://, got '{url}'"
        )))));
    }

    let timeout_ms = vm_get_int_option(options, "timeout", 30_000).max(0) as u64;
    let retries = vm_get_int_option(options, "retries", 0).max(0) as u32;
    let backoff_ms = vm_get_int_option(options, "backoff", 1000).max(0) as u64;
    let follow_redirects = vm_get_bool_option(options, "follow_redirects", true);
    let max_redirects = vm_get_int_option(options, "max_redirects", 10).max(0) as usize;

    let redirect_policy = if follow_redirects {
        reqwest::redirect::Policy::limited(max_redirects)
    } else {
        reqwest::redirect::Policy::none()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
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

    // Apply auth
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

    // Apply explicit headers
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

    let mut last_err: Option<VmError> = None;
    let total_attempts = 1 + retries;

    for attempt in 0..total_attempts {
        if attempt > 0 {
            use rand::Rng;
            let base_delay = backoff_ms.saturating_mul(1u64 << (attempt - 1).min(30));
            let jitter: f64 = rand::thread_rng().gen_range(0.75..=1.25);
            let delay_ms = ((base_delay as f64 * jitter) as u64).min(60_000);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let mut req = client.request(req_method.clone(), url);
        req = req.headers(header_map.clone());
        if let Some(ref b) = body_str {
            req = req.body(b.clone());
        }

        match req.send().await {
            Ok(response) => {
                let status_code = response.status().as_u16();
                let ok = (200..300).contains(&status_code);
                let status = status_code as i64;

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

                if status >= 500 && attempt + 1 < total_attempts {
                    last_err = Some(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: server error {status}"
                    )))));
                    continue;
                }

                let mut result = BTreeMap::new();
                result.insert("status".to_string(), VmValue::Int(status));
                result.insert("headers".to_string(), VmValue::Dict(Rc::new(resp_headers)));
                result.insert(
                    "body".to_string(),
                    VmValue::String(Rc::from(body_text.as_str())),
                );
                result.insert("ok".to_string(), VmValue::Bool(ok));
                return Ok(VmValue::Dict(Rc::new(result)));
            }
            Err(e) => {
                let retryable = e.is_timeout() || e.is_connect();
                if retryable && attempt + 1 < total_attempts {
                    last_err = Some(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: request failed: {e}"
                    )))));
                    continue;
                }
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: request failed: {e}"
                )))));
            }
        }
    }

    Err(last_err
        .unwrap_or_else(|| VmError::Thrown(VmValue::String(Rc::from("http: request failed")))))
}
