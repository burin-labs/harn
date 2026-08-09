use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::value::VmValue;

#[derive(Clone, Debug)]
pub(super) struct MockResponse {
    pub(super) status: i64,
    pub(super) body: String,
    pub(super) headers: crate::value::DictMap,
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
                .map(|(key, value)| {
                    (
                        crate::value::intern_key(&key),
                        VmValue::String(arcstr::ArcStr::from(value)),
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
struct HttpMock {
    method: String,
    url_pattern: String,
    responses: Vec<MockResponse>,
    next_response: usize,
}

#[derive(Clone, Debug)]
struct HttpMockCall {
    method: String,
    url: String,
    headers: crate::value::DictMap,
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

/// HTTP fixture state bound to one typed `Harness`.
///
/// The legacy ambient mock registry is thread-local. A typed harness can cross
/// executor threads between fixture registration and an awaited request, so
/// its deterministic transport state must travel with the harness instead.
#[derive(Debug, Default)]
pub(crate) struct HttpMockRegistry {
    inner: Mutex<HttpMockRegistryInner>,
}

#[derive(Debug, Default)]
struct HttpMockRegistryInner {
    mocks: Vec<HttpMock>,
    calls: Vec<HttpMockCall>,
}

impl HttpMockRegistry {
    pub(crate) fn clear(&self) {
        let mut inner = self.inner.lock().expect("harness HTTP mocks poisoned");
        inner.mocks.clear();
        inner.calls.clear();
    }

    pub(crate) fn has_match(&self, method: &str, url: &str) -> bool {
        self.inner
            .lock()
            .expect("harness HTTP mocks poisoned")
            .mocks
            .iter()
            .any(|mock| {
                !mock.responses.is_empty()
                    && (mock.method == "*" || mock.method.eq_ignore_ascii_case(method))
                    && url_matches(&mock.url_pattern, url)
            })
    }

    pub(super) fn register(
        &self,
        method: impl Into<String>,
        url_pattern: impl Into<String>,
        responses: Vec<MockResponse>,
    ) {
        let method = method.into();
        let url_pattern = url_pattern.into();
        let mut inner = self.inner.lock().expect("harness HTTP mocks poisoned");
        inner
            .mocks
            .retain(|mock| !(mock.method == method && mock.url_pattern == url_pattern));
        inner.mocks.push(HttpMock {
            method,
            url_pattern,
            responses,
            next_response: 0,
        });
    }

    pub(super) fn consume(
        &self,
        method: &str,
        url: &str,
        headers: crate::value::DictMap,
        body: Option<String>,
    ) -> Option<MockResponse> {
        let mut inner = self.inner.lock().expect("harness HTTP mocks poisoned");
        let response = inner.mocks.iter_mut().find_map(|mock| {
            if (mock.method == "*" || mock.method.eq_ignore_ascii_case(method))
                && url_matches(&mock.url_pattern, url)
            {
                let last_index = mock.responses.len().checked_sub(1)?;
                let index = mock.next_response.min(last_index);
                let response = mock.responses[index].clone();
                if mock.next_response < last_index {
                    mock.next_response += 1;
                }
                Some(response)
            } else {
                None
            }
        })?;
        inner.calls.push(HttpMockCall {
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body,
        });
        Some(response)
    }

    pub(crate) fn calls_value(&self, redact_sensitive: bool) -> Vec<VmValue> {
        let inner = self.inner.lock().expect("harness HTTP mocks poisoned");
        http_mock_calls_from(&inner.calls, redact_sensitive)
    }
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
                    .map(|(key, value)| (key.to_string(), value.display()))
                    .collect(),
                body: call.body.clone(),
            })
            .collect()
    })
}

pub(super) fn http_mock_calls_value(redact_sensitive: bool) -> Vec<VmValue> {
    HTTP_MOCK_CALLS.with(|calls| http_mock_calls_from(&calls.borrow(), redact_sensitive))
}

fn http_mock_calls_from(calls: &[HttpMockCall], redact_sensitive: bool) -> Vec<VmValue> {
    calls
        .iter()
        .map(|call| {
            let mut dict = BTreeMap::new();
            dict.put_str("method", call.method.as_str());
            dict.put_str("url", redact_mock_call_url(&call.url, redact_sensitive));
            dict.insert(
                "headers".to_string(),
                VmValue::dict(mock_call_headers_value(&call.headers, redact_sensitive)),
            );
            dict.insert(
                "body".to_string(),
                match &call.body {
                    Some(body) => VmValue::String(arcstr::ArcStr::from(body.as_str())),
                    None => VmValue::Nil,
                },
            );
            VmValue::dict(dict)
        })
        .collect()
}

pub(super) fn parse_mock_responses(response: &crate::value::DictMap) -> Vec<MockResponse> {
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

fn parse_mock_response_dict(response: &crate::value::DictMap) -> MockResponse {
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
    headers: crate::value::DictMap,
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
            match remaining.strip_prefix(*part) {
                Some(rest) => remaining = rest,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            if !remaining.ends_with(part) {
                return false;
            }
            remaining = "";
        } else {
            match remaining.split_once(*part) {
                Some((_, rest)) => remaining = rest,
                None => return false,
            }
        }
    }
    true
}

pub(super) fn redact_mock_call_url(url: &str, redact: bool) -> String {
    if !redact {
        return url.to_string();
    }
    crate::redact::current_policy().redact_url(url)
}

pub(super) fn mock_call_headers_value(
    headers: &crate::value::DictMap,
    redact_headers: bool,
) -> crate::value::DictMap {
    if !redact_headers {
        return headers.clone();
    }
    let policy = crate::redact::current_policy();
    headers
        .iter()
        .map(|(key, value)| {
            let value = if policy.header_is_sensitive(key) {
                VmValue::String(arcstr::ArcStr::from(crate::redact::REDACTED_PLACEHOLDER))
            } else {
                value.clone()
            };
            (key.clone(), value)
        })
        .collect()
}
