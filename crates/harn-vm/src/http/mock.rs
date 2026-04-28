use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

#[derive(Clone)]
pub(super) struct MockResponse {
    pub(super) status: i64,
    pub(super) body: String,
    pub(super) headers: BTreeMap<String, VmValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpMockResponse {
    pub status: i64,
    pub body: String,
    pub headers: BTreeMap<String, String>,
}

impl HttpMockResponse {
    pub fn new(status: i64, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            headers: BTreeMap::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

impl From<HttpMockResponse> for MockResponse {
    fn from(value: HttpMockResponse) -> Self {
        Self {
            status: value.status,
            body: value.body,
            headers: value
                .headers
                .into_iter()
                .map(|(key, value)| (key, VmValue::String(Rc::from(value))))
                .collect(),
        }
    }
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
    headers: BTreeMap<String, VmValue>,
    body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpMockCallSnapshot {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

thread_local! {
    static HTTP_MOCKS: RefCell<Vec<HttpMock>> = const { RefCell::new(Vec::new()) };
    static HTTP_MOCK_CALLS: RefCell<Vec<HttpMockCall>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn reset_http_mocks() {
    HTTP_MOCKS.with(|mocks| mocks.borrow_mut().clear());
    HTTP_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
}

pub(super) fn clear_http_mocks() {
    reset_http_mocks();
}

pub fn push_http_mock(
    method: impl Into<String>,
    url_pattern: impl Into<String>,
    responses: Vec<HttpMockResponse>,
) {
    let responses = if responses.is_empty() {
        vec![MockResponse::from(HttpMockResponse::new(200, ""))]
    } else {
        responses.into_iter().map(MockResponse::from).collect()
    };
    register_http_mock(method.into(), url_pattern.into(), responses);
}

pub(super) fn register_http_mock(
    method: impl Into<String>,
    url_pattern: impl Into<String>,
    responses: Vec<MockResponse>,
) {
    let method = method.into();
    let url_pattern = url_pattern.into();
    HTTP_MOCKS.with(|mocks| {
        let mut mocks = mocks.borrow_mut();
        // Re-registering the same (method, url_pattern) replaces the prior
        // mock so tests can override per-case responses without first calling
        // http_mock_clear(). Without this, the original mock keeps matching
        // forever and the new one is dead.
        mocks.retain(|mock| !(mock.method == method && mock.url_pattern == url_pattern));
        mocks.push(HttpMock {
            method,
            url_pattern,
            responses,
            next_response: 0,
        });
    });
}

pub fn http_mock_calls_snapshot() -> Vec<HttpMockCallSnapshot> {
    HTTP_MOCK_CALLS.with(|calls| {
        calls
            .borrow()
            .iter()
            .map(|call| HttpMockCallSnapshot {
                method: call.method.clone(),
                url: call.url.clone(),
                headers: call
                    .headers
                    .iter()
                    .map(|(key, value)| (key.clone(), value.display()))
                    .collect(),
                body: call.body.clone(),
            })
            .collect()
    })
}

pub(super) fn http_mock_calls_value(redact_sensitive: bool) -> Vec<VmValue> {
    HTTP_MOCK_CALLS.with(|calls| {
        calls
            .borrow()
            .iter()
            .map(|call| {
                let mut dict = BTreeMap::new();
                dict.insert(
                    "method".to_string(),
                    VmValue::String(Rc::from(call.method.as_str())),
                );
                dict.insert(
                    "url".to_string(),
                    VmValue::String(Rc::from(redact_mock_call_url(&call.url, redact_sensitive))),
                );
                dict.insert(
                    "headers".to_string(),
                    VmValue::Dict(Rc::new(mock_call_headers_value(
                        &call.headers,
                        redact_sensitive,
                    ))),
                );
                dict.insert(
                    "body".to_string(),
                    match &call.body {
                        Some(body) => VmValue::String(Rc::from(body.as_str())),
                        None => VmValue::Nil,
                    },
                );
                VmValue::Dict(Rc::new(dict))
            })
            .collect()
    })
}

pub(super) fn parse_mock_responses(response: &BTreeMap<String, VmValue>) -> Vec<MockResponse> {
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

pub(super) fn consume_http_mock(
    method: &str,
    url: &str,
    headers: BTreeMap<String, VmValue>,
    body: Option<String>,
) -> Option<MockResponse> {
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
            headers,
            body,
        });
    });

    Some(response)
}

/// Check if a URL matches a mock pattern (exact or glob with `*`).
pub(super) fn url_matches(pattern: &str, url: &str) -> bool {
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

fn is_sensitive_http_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
            | "x-csrf-token"
            | "x-xsrf-token"
    )
}

fn is_sensitive_url_param(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized == "api_key"
        || normalized == "apikey"
        || normalized == "access_token"
        || normalized == "refresh_token"
        || normalized == "id_token"
        || normalized == "client_secret"
        || normalized == "password"
        || normalized == "secret"
        || normalized == "token"
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
}

pub(super) fn redact_mock_call_url(url: &str, redact: bool) -> String {
    if !redact {
        return url.to_string();
    }
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    let mut redacted_any = false;
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let value = if is_sensitive_url_param(&key) {
                redacted_any = true;
                "[redacted]".to_string()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    if !redacted_any {
        return url.to_string();
    }
    parsed.set_query(None);
    {
        let mut query = parsed.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
    }
    parsed.to_string()
}

pub(super) fn mock_call_headers_value(
    headers: &BTreeMap<String, VmValue>,
    redact_headers: bool,
) -> BTreeMap<String, VmValue> {
    headers
        .iter()
        .map(|(key, value)| {
            let value = if redact_headers && is_sensitive_http_header(key) {
                VmValue::String(Rc::from("[redacted]"))
            } else {
                value.clone()
            };
            (key.clone(), value)
        })
        .collect()
}
