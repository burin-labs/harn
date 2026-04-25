use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

use base64::Engine;
use futures::{SinkExt, StreamExt};
use reqwest_eventsource::{Event as SseEvent, EventSource};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use x509_parser::prelude::{FromDer, X509Certificate};

// Mock HTTP framework (thread-local, mirrors the mock LLM pattern).

#[derive(Clone)]
struct MockResponse {
    status: i64,
    body: String,
    headers: BTreeMap<String, VmValue>,
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
    total_timeout_ms: u64,
    connect_timeout_ms: Option<u64>,
    read_timeout_ms: Option<u64>,
    retry: RetryConfig,
    follow_redirects: bool,
    max_redirects: usize,
    proxy: Option<HttpProxyConfig>,
    tls: HttpTlsConfig,
    decompress: bool,
}

#[derive(Clone, Default)]
struct HttpTlsConfig {
    ca_bundle_path: Option<String>,
    client_cert_path: Option<String>,
    client_key_path: Option<String>,
    client_identity_path: Option<String>,
    pinned_sha256: Vec<String>,
}

#[derive(Clone)]
struct HttpProxyConfig {
    url: String,
    auth: Option<(String, String)>,
    no_proxy: Option<String>,
}

#[derive(Clone)]
struct HttpSession {
    client: reqwest::Client,
    options: BTreeMap<String, VmValue>,
}

struct HttpRequestParts {
    method: reqwest::Method,
    headers: reqwest::header::HeaderMap,
    recorded_headers: BTreeMap<String, VmValue>,
    body: Option<String>,
    multipart: Option<MultipartRequest>,
}

#[derive(Clone)]
struct MultipartRequest {
    parts: Vec<MultipartField>,
    mock_body: String,
}

#[derive(Clone)]
struct MultipartField {
    name: String,
    value: Vec<u8>,
    filename: Option<String>,
    content_type: Option<String>,
}

struct HttpStreamHandle {
    kind: HttpStreamKind,
    status: i64,
    headers: BTreeMap<String, VmValue>,
    pending: VecDeque<u8>,
    closed: bool,
}

enum HttpStreamKind {
    Real(Rc<tokio::sync::Mutex<reqwest::Response>>),
    Fake,
}

struct SseMock {
    url_pattern: String,
    events: Vec<MockStreamEvent>,
}

#[derive(Clone)]
struct MockStreamEvent {
    event_type: String,
    data: String,
    id: Option<String>,
    retry_ms: Option<i64>,
}

struct SseHandle {
    kind: SseHandleKind,
    url: String,
    max_events: usize,
    max_message_bytes: usize,
    received: usize,
}

enum SseHandleKind {
    Real(Rc<tokio::sync::Mutex<EventSource>>),
    Fake(Rc<tokio::sync::Mutex<FakeSseStream>>),
}

struct FakeSseStream {
    events: VecDeque<MockStreamEvent>,
    opened: bool,
    closed: bool,
}

struct WebSocketMock {
    url_pattern: String,
    messages: Vec<MockWsMessage>,
    echo: bool,
}

#[derive(Clone)]
struct MockWsMessage {
    message_type: String,
    data: Vec<u8>,
}

type RealWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct WebSocketHandle {
    kind: WebSocketHandleKind,
    url: String,
    max_messages: usize,
    max_message_bytes: usize,
    received: usize,
}

enum WebSocketHandleKind {
    Real(Rc<tokio::sync::Mutex<RealWebSocket>>),
    Fake(Rc<tokio::sync::Mutex<FakeWebSocket>>),
}

struct FakeWebSocket {
    messages: VecDeque<MockWsMessage>,
    echo: bool,
    closed: bool,
}

#[derive(Clone)]
struct TransportMockCall {
    kind: String,
    handle: Option<String>,
    url: String,
    message_type: Option<String>,
    data: Option<String>,
}

#[derive(Clone)]
struct HttpServerRoute {
    method: String,
    template: String,
    handler: Rc<VmClosure>,
    max_body_bytes: Option<usize>,
    retain_raw_body: Option<bool>,
}

#[derive(Clone)]
struct HttpServer {
    routes: Vec<HttpServerRoute>,
    before: Vec<Rc<VmClosure>>,
    after: Vec<Rc<VmClosure>>,
    ready: bool,
    readiness: Option<Rc<VmClosure>>,
    shutdown_hooks: Vec<Rc<VmClosure>>,
    shutdown: bool,
    max_body_bytes: usize,
    retain_raw_body: bool,
}

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_BACKOFF_MS: u64 = 1_000;
const MAX_RETRY_DELAY_MS: u64 = 60_000;
const DEFAULT_RETRYABLE_STATUSES: [u16; 6] = [408, 429, 500, 502, 503, 504];
const DEFAULT_RETRYABLE_METHODS: [&str; 5] = ["GET", "HEAD", "PUT", "DELETE", "OPTIONS"];
const DEFAULT_TRANSPORT_RECEIVE_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_STREAM_EVENTS: usize = 10_000;
const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const DEFAULT_SERVER_MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_SESSIONS: usize = 64;
const MAX_HTTP_STREAMS: usize = 64;
const MAX_SSE_STREAMS: usize = 64;
const MAX_WEBSOCKETS: usize = 64;
const MULTIPART_MOCK_BOUNDARY: &str = "harn-boundary";
const MAX_HTTP_SERVERS: usize = 128;

thread_local! {
    static HTTP_MOCKS: RefCell<Vec<HttpMock>> = const { RefCell::new(Vec::new()) };
    static HTTP_MOCK_CALLS: RefCell<Vec<HttpMockCall>> = const { RefCell::new(Vec::new()) };
    static HTTP_CLIENTS: RefCell<HashMap<String, reqwest::Client>> = RefCell::new(HashMap::new());
    static HTTP_SESSIONS: RefCell<HashMap<String, HttpSession>> = RefCell::new(HashMap::new());
    static HTTP_STREAMS: RefCell<HashMap<String, HttpStreamHandle>> = RefCell::new(HashMap::new());
    static SSE_MOCKS: RefCell<Vec<SseMock>> = const { RefCell::new(Vec::new()) };
    static SSE_HANDLES: RefCell<HashMap<String, SseHandle>> = RefCell::new(HashMap::new());
    static WEBSOCKET_MOCKS: RefCell<Vec<WebSocketMock>> = const { RefCell::new(Vec::new()) };
    static WEBSOCKET_HANDLES: RefCell<HashMap<String, WebSocketHandle>> = RefCell::new(HashMap::new());
    static TRANSPORT_MOCK_CALLS: RefCell<Vec<TransportMockCall>> = const { RefCell::new(Vec::new()) };
    static TRANSPORT_HANDLE_COUNTER: RefCell<u64> = const { RefCell::new(0) };
    static HTTP_SERVERS: RefCell<HashMap<String, HttpServer>> = RefCell::new(HashMap::new());
}

/// Reset thread-local HTTP mock state. Call between test runs.
pub fn reset_http_state() {
    HTTP_MOCKS.with(|m| m.borrow_mut().clear());
    HTTP_MOCK_CALLS.with(|c| c.borrow_mut().clear());
    HTTP_CLIENTS.with(|clients| clients.borrow_mut().clear());
    HTTP_SESSIONS.with(|sessions| sessions.borrow_mut().clear());
    HTTP_STREAMS.with(|streams| streams.borrow_mut().clear());
    SSE_MOCKS.with(|mocks| mocks.borrow_mut().clear());
    SSE_HANDLES.with(|handles| {
        for handle in handles.borrow_mut().values_mut() {
            if let SseHandleKind::Real(stream) = &handle.kind {
                if let Ok(mut stream) = stream.try_lock() {
                    stream.close();
                }
            }
        }
        handles.borrow_mut().clear();
    });
    WEBSOCKET_MOCKS.with(|mocks| mocks.borrow_mut().clear());
    WEBSOCKET_HANDLES.with(|handles| handles.borrow_mut().clear());
    TRANSPORT_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
    TRANSPORT_HANDLE_COUNTER.with(|counter| *counter.borrow_mut() = 0);
    HTTP_SERVERS.with(|servers| servers.borrow_mut().clear());
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

fn build_http_download_response(
    status: i64,
    headers: BTreeMap<String, VmValue>,
    bytes_written: u64,
) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("status".to_string(), VmValue::Int(status));
    result.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
    result.insert(
        "bytes_written".to_string(),
        VmValue::Int(bytes_written as i64),
    );
    result.insert(
        "ok".to_string(),
        VmValue::Bool((200..300).contains(&(status as u16))),
    );
    VmValue::Dict(Rc::new(result))
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, VmValue> {
    let mut resp_headers = BTreeMap::new();
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            resp_headers.insert(name.as_str().to_string(), VmValue::String(Rc::from(v)));
        }
    }
    resp_headers
}

fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

fn next_transport_handle(prefix: &str) -> String {
    TRANSPORT_HANDLE_COUNTER.with(|counter| {
        let mut counter = counter.borrow_mut();
        *counter += 1;
        format!("{prefix}-{}", *counter)
    })
}

fn handle_from_value(value: &VmValue, builtin: &str) -> Result<String, VmError> {
    match value {
        VmValue::String(handle) => Ok(handle.to_string()),
        VmValue::Dict(dict) => dict
            .get("id")
            .map(|id| id.display())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| vm_error(format!("{builtin}: handle dict must contain id"))),
        _ => Err(vm_error(format!(
            "{builtin}: first argument must be a handle string or dict"
        ))),
    }
}

fn get_options_arg(args: &[VmValue], index: usize) -> BTreeMap<String, VmValue> {
    args.get(index)
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default()
}

fn merge_options(
    base: &BTreeMap<String, VmValue>,
    overrides: &BTreeMap<String, VmValue>,
) -> BTreeMap<String, VmValue> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn resolve_http_path(
    builtin: &str,
    path: &str,
    access: crate::stdlib::sandbox::FsAccess,
) -> Result<std::path::PathBuf, VmError> {
    let resolved = crate::stdlib::process::resolve_source_relative_path(path);
    crate::stdlib::sandbox::enforce_fs_path(builtin, &resolved, access)?;
    Ok(resolved)
}

fn value_to_bytes(value: &VmValue) -> Vec<u8> {
    match value {
        VmValue::Bytes(bytes) => bytes.as_ref().clone(),
        other => other.display().into_bytes(),
    }
}

fn parse_multipart_field(value: &VmValue) -> Result<MultipartField, VmError> {
    let dict = value
        .as_dict()
        .ok_or_else(|| vm_error("http: multipart entries must be dicts"))?;
    let name = dict
        .get("name")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| vm_error("http: multipart entry requires name"))?;
    let content_type = dict
        .get("content_type")
        .or_else(|| dict.get("mime_type"))
        .map(|value| value.display())
        .filter(|value| !value.is_empty());

    let mut filename = dict
        .get("filename")
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    let value = if let Some(path_value) = dict.get("path") {
        let path = path_value.display();
        let resolved = resolve_http_path(
            "http multipart",
            &path,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        if filename.is_none() {
            filename = resolved
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string());
        }
        std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read multipart file {}: {error}",
                resolved.display()
            ))
        })?
    } else if let Some(base64_value) = dict.get("value_base64").or_else(|| dict.get("base64")) {
        base64::engine::general_purpose::STANDARD
            .decode(base64_value.display())
            .map_err(|error| vm_error(format!("http: invalid multipart base64 value: {error}")))?
    } else {
        dict.get("value").map(value_to_bytes).ok_or_else(|| {
            vm_error("http: multipart entry requires value, value_base64, or path")
        })?
    };

    Ok(MultipartField {
        name,
        value,
        filename,
        content_type,
    })
}

fn parse_multipart_request(
    options: &BTreeMap<String, VmValue>,
) -> Result<Option<MultipartRequest>, VmError> {
    let Some(value) = options.get("multipart") else {
        return Ok(None);
    };
    let VmValue::List(items) = value else {
        return Err(vm_error("http: multipart must be a list"));
    };
    let parts = items
        .iter()
        .map(parse_multipart_field)
        .collect::<Result<Vec<_>, _>>()?;
    let mock_body = multipart_mock_body(&parts);
    Ok(Some(MultipartRequest { parts, mock_body }))
}

fn multipart_mock_body(parts: &[MultipartField]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str("--");
        out.push_str(MULTIPART_MOCK_BOUNDARY);
        out.push_str("\r\nContent-Disposition: form-data; name=\"");
        out.push_str(&part.name);
        out.push('"');
        if let Some(filename) = &part.filename {
            out.push_str("; filename=\"");
            out.push_str(filename);
            out.push('"');
        }
        out.push_str("\r\n");
        if let Some(content_type) = &part.content_type {
            out.push_str("Content-Type: ");
            out.push_str(content_type);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(&String::from_utf8_lossy(&part.value));
        out.push_str("\r\n");
    }
    out.push_str("--");
    out.push_str(MULTIPART_MOCK_BOUNDARY);
    out.push_str("--\r\n");
    out
}

fn multipart_form(request: &MultipartRequest) -> Result<reqwest::multipart::Form, VmError> {
    let mut form = reqwest::multipart::Form::new();
    for field in &request.parts {
        let mut part = reqwest::multipart::Part::bytes(field.value.clone());
        if let Some(filename) = &field.filename {
            part = part.file_name(filename.clone());
        }
        if let Some(content_type) = &field.content_type {
            part = part.mime_str(content_type).map_err(|error| {
                vm_error(format!("http: invalid multipart content_type: {error}"))
            })?;
        }
        form = form.part(field.name.clone(), part);
    }
    Ok(form)
}

fn transport_limit_option(options: &BTreeMap<String, VmValue>, key: &str, default: usize) -> usize {
    options
        .get(key)
        .and_then(|value| value.as_int())
        .map(|value| value.max(0) as usize)
        .unwrap_or(default)
}

fn receive_timeout_arg(args: &[VmValue], index: usize) -> u64 {
    match args.get(index) {
        Some(VmValue::Duration(ms)) => (*ms).max(0) as u64,
        Some(value) => value
            .as_int()
            .map(|ms| ms.max(0) as u64)
            .unwrap_or(DEFAULT_TRANSPORT_RECEIVE_TIMEOUT_MS),
        None => DEFAULT_TRANSPORT_RECEIVE_TIMEOUT_MS,
    }
}

fn timeout_event() -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("type".to_string(), VmValue::String(Rc::from("timeout")));
    VmValue::Dict(Rc::new(dict))
}

fn closed_event() -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("type".to_string(), VmValue::String(Rc::from("close")));
    VmValue::Dict(Rc::new(dict))
}

fn record_transport_call(call: TransportMockCall) {
    TRANSPORT_MOCK_CALLS.with(|calls| calls.borrow_mut().push(call));
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
        match (args.get(1), args.get(2)) {
            (Some(VmValue::Dict(d)), None) => (**d).clone(),
            (_, Some(VmValue::Dict(d))) => (**d).clone(),
            _ => BTreeMap::new(),
        }
    } else {
        match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        }
    };
    if has_body && !(matches!(args.get(1), Some(VmValue::Dict(_))) && args.get(2).is_none()) {
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

fn consume_http_mock(
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

fn vm_string(value: impl AsRef<str>) -> VmValue {
    VmValue::String(Rc::from(value.as_ref()))
}

fn dict_value(entries: BTreeMap<String, VmValue>) -> VmValue {
    VmValue::Dict(Rc::new(entries))
}

fn get_bool_option(options: &BTreeMap<String, VmValue>, key: &str, default: bool) -> bool {
    match options.get(key) {
        Some(VmValue::Bool(value)) => *value,
        _ => default,
    }
}

fn get_usize_option(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    default: usize,
) -> Result<usize, VmError> {
    match options.get(key).and_then(VmValue::as_int) {
        Some(value) if value >= 0 => Ok(value as usize),
        Some(_) => Err(vm_error(format!("http_server: {key} must be non-negative"))),
        None => Ok(default),
    }
}

fn get_optional_usize_option(
    options: &BTreeMap<String, VmValue>,
    key: &str,
) -> Result<Option<usize>, VmError> {
    match options.get(key).and_then(VmValue::as_int) {
        Some(value) if value >= 0 => Ok(Some(value as usize)),
        Some(_) => Err(vm_error(format!(
            "http_server_route: {key} must be non-negative"
        ))),
        None => Ok(None),
    }
}

fn server_from_value(value: &VmValue, builtin: &str) -> Result<String, VmError> {
    handle_from_value(value, builtin)
}

fn closure_arg(args: &[VmValue], index: usize, builtin: &str) -> Result<Rc<VmClosure>, VmError> {
    match args.get(index) {
        Some(VmValue::Closure(closure)) => Ok(closure.clone()),
        Some(other) => Err(vm_error(format!(
            "{builtin}: argument {} must be a closure, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(vm_error(format!(
            "{builtin}: missing closure argument {}",
            index + 1
        ))),
    }
}

fn http_server_handle_value(id: &str) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("id".to_string(), vm_string(id));
    dict.insert("kind".to_string(), vm_string("http_server"));
    dict_value(dict)
}

fn header_lookup_value(headers: &BTreeMap<String, VmValue>, name: &str) -> VmValue {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .unwrap_or(VmValue::Nil)
}

fn headers_from_value(value: &VmValue) -> BTreeMap<String, VmValue> {
    match value {
        VmValue::Dict(dict) => dict
            .get("headers")
            .and_then(VmValue::as_dict)
            .map(|headers| {
                headers
                    .iter()
                    .map(|(key, value)| (key.to_ascii_lowercase(), vm_string(value.display())))
                    .collect()
            })
            .unwrap_or_else(|| {
                dict.iter()
                    .map(|(key, value)| (key.to_ascii_lowercase(), vm_string(value.display())))
                    .collect()
            }),
        _ => BTreeMap::new(),
    }
}

fn normalize_headers(value: Option<&VmValue>) -> BTreeMap<String, VmValue> {
    match value.and_then(VmValue::as_dict) {
        Some(headers) => headers
            .iter()
            .map(|(key, value)| (key.to_ascii_lowercase(), vm_string(value.display())))
            .collect(),
        None => BTreeMap::new(),
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn split_path_and_query(raw_path: &str) -> (String, BTreeMap<String, VmValue>) {
    let (path, query) = raw_path.split_once('?').unwrap_or((raw_path, ""));
    let mut query_map = BTreeMap::new();
    for pair in query.split('&').filter(|part| !part.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query_map.insert(percent_decode(key), vm_string(percent_decode(value)));
    }
    (
        if path.is_empty() { "/" } else { path }.to_string(),
        query_map,
    )
}

fn request_body_bytes(input: &BTreeMap<String, VmValue>) -> Vec<u8> {
    match input.get("raw_body").or_else(|| input.get("body")) {
        Some(VmValue::Bytes(bytes)) => bytes.as_ref().clone(),
        Some(value) => value.display().into_bytes(),
        None => Vec::new(),
    }
}

fn request_value(
    method: &str,
    path: &str,
    path_params: BTreeMap<String, VmValue>,
    mut query: BTreeMap<String, VmValue>,
    input: &BTreeMap<String, VmValue>,
    body_bytes: &[u8],
    retain_raw_body: bool,
) -> VmValue {
    if let Some(explicit_query) = input.get("query").and_then(VmValue::as_dict) {
        query.extend(
            explicit_query
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }

    let headers = normalize_headers(input.get("headers"));
    let body = String::from_utf8_lossy(body_bytes).into_owned();
    let mut request = BTreeMap::new();
    request.insert("method".to_string(), vm_string(method));
    request.insert("path".to_string(), vm_string(path));
    let path_params = dict_value(path_params);
    request.insert("path_params".to_string(), path_params.clone());
    request.insert("params".to_string(), path_params);
    request.insert("query".to_string(), dict_value(query));
    request.insert("headers".to_string(), dict_value(headers));
    request.insert("body".to_string(), vm_string(body));
    request.insert(
        "raw_body".to_string(),
        if retain_raw_body {
            VmValue::Bytes(Rc::new(body_bytes.to_vec()))
        } else {
            VmValue::Nil
        },
    );
    request.insert(
        "body_bytes".to_string(),
        VmValue::Int(body_bytes.len() as i64),
    );
    request.insert(
        "remote_addr".to_string(),
        input
            .get("remote_addr")
            .or_else(|| input.get("remote"))
            .map(|value| vm_string(value.display()))
            .unwrap_or(VmValue::Nil),
    );
    request.insert(
        "client_ip".to_string(),
        input
            .get("client_ip")
            .or_else(|| input.get("remote_ip"))
            .or_else(|| input.get("ip"))
            .map(|value| vm_string(value.display()))
            .unwrap_or(VmValue::Nil),
    );
    dict_value(request)
}

fn normalize_status(status: i64) -> i64 {
    if (100..=999).contains(&status) {
        status
    } else {
        500
    }
}

fn response_with_kind(
    status: i64,
    mut headers: BTreeMap<String, VmValue>,
    body: VmValue,
    body_kind: &str,
) -> VmValue {
    let status = normalize_status(status);
    let mut response = BTreeMap::new();
    if body_kind == "json" && matches!(header_lookup_value(&headers, "content-type"), VmValue::Nil)
    {
        headers.insert(
            "content-type".to_string(),
            vm_string("application/json; charset=utf-8"),
        );
    } else if body_kind == "text"
        && matches!(header_lookup_value(&headers, "content-type"), VmValue::Nil)
    {
        headers.insert(
            "content-type".to_string(),
            vm_string("text/plain; charset=utf-8"),
        );
    }
    response.insert("status".to_string(), VmValue::Int(status));
    response.insert("headers".to_string(), dict_value(headers));
    response.insert(
        "ok".to_string(),
        VmValue::Bool((200..300).contains(&status)),
    );
    response.insert("body_kind".to_string(), vm_string(body_kind));
    match body {
        VmValue::Bytes(bytes) => {
            response.insert(
                "body".to_string(),
                vm_string(String::from_utf8_lossy(&bytes)),
            );
            response.insert("raw_body".to_string(), VmValue::Bytes(bytes));
        }
        other => {
            response.insert("body".to_string(), vm_string(other.display()));
            response.insert(
                "raw_body".to_string(),
                VmValue::Bytes(Rc::new(other.display().into_bytes())),
            );
        }
    }
    dict_value(response)
}

fn normalize_response(value: VmValue) -> VmValue {
    match value {
        VmValue::Dict(dict) if dict.contains_key("status") => {
            let status = dict.get("status").and_then(VmValue::as_int).unwrap_or(200);
            let headers = dict
                .get("headers")
                .and_then(VmValue::as_dict)
                .cloned()
                .unwrap_or_default();
            let body_kind = dict
                .get("body_kind")
                .or_else(|| dict.get("kind"))
                .map(|value| value.display())
                .unwrap_or_else(|| "text".to_string());
            let body = dict
                .get("raw_body")
                .filter(|value| matches!(value, VmValue::Bytes(_)))
                .or_else(|| dict.get("body"))
                .cloned()
                .unwrap_or(VmValue::Nil);
            response_with_kind(status, headers, body, &body_kind)
        }
        VmValue::Nil => response_with_kind(204, BTreeMap::new(), VmValue::Nil, "text"),
        other => response_with_kind(200, BTreeMap::new(), other, "text"),
    }
}

fn body_limit_response(limit: usize, actual: usize) -> VmValue {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        vm_string("text/plain; charset=utf-8"),
    );
    headers.insert("connection".to_string(), vm_string("close"));
    headers.insert(
        "x-harn-body-limit".to_string(),
        vm_string(limit.to_string()),
    );
    response_with_kind(
        413,
        headers,
        vm_string(format!("request body too large: {actual} > {limit} bytes")),
        "text",
    )
}

fn not_found_response(method: &str, path: &str) -> VmValue {
    response_with_kind(
        404,
        BTreeMap::new(),
        vm_string(format!("no route for {method} {path}")),
        "text",
    )
}

fn unavailable_response(message: &str) -> VmValue {
    response_with_kind(503, BTreeMap::new(), vm_string(message), "text")
}

fn route_template_match(template: &str, path: &str) -> Option<BTreeMap<String, VmValue>> {
    let template_segments: Vec<&str> = template.trim_matches('/').split('/').collect();
    let path_segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if template == "/" && path == "/" {
        return Some(BTreeMap::new());
    }
    if template_segments.len() != path_segments.len() {
        return None;
    }
    let mut params = BTreeMap::new();
    for (tmpl, actual) in template_segments.iter().zip(path_segments.iter()) {
        if tmpl.starts_with('{') && tmpl.ends_with('}') && tmpl.len() > 2 {
            params.insert(
                tmpl[1..tmpl.len() - 1].to_string(),
                vm_string(percent_decode(actual)),
            );
        } else if tmpl.starts_with(':') && tmpl.len() > 1 {
            params.insert(tmpl[1..].to_string(), vm_string(percent_decode(actual)));
        } else if tmpl != actual {
            return None;
        }
    }
    Some(params)
}

fn matching_route(
    server: &HttpServer,
    method: &str,
    path: &str,
) -> Option<(HttpServerRoute, BTreeMap<String, VmValue>)> {
    server.routes.iter().find_map(|route| {
        if route.method != "*" && !route.method.eq_ignore_ascii_case(method) {
            return None;
        }
        route_template_match(&route.template, path).map(|params| (route.clone(), params))
    })
}

async fn call_server_closure(
    closure: &Rc<VmClosure>,
    args: &[VmValue],
    builtin: &str,
) -> Result<VmValue, VmError> {
    let mut vm = crate::vm::clone_async_builtin_child_vm()
        .ok_or_else(|| vm_error(format!("{builtin}: requires an async builtin VM context")))?;
    vm.call_closure_pub(closure, args).await
}

fn value_is_response(value: &VmValue) -> bool {
    matches!(value, VmValue::Dict(dict) if dict.contains_key("status"))
}

async fn run_http_server_request(server_id: &str, request: VmValue) -> Result<VmValue, VmError> {
    let server = HTTP_SERVERS.with(|servers| servers.borrow().get(server_id).cloned());
    let Some(server) = server else {
        return Err(vm_error(format!(
            "http_server_request: unknown server handle '{server_id}'"
        )));
    };
    if server.shutdown {
        return Ok(unavailable_response("server is shut down"));
    }
    if !server.ready {
        return Ok(unavailable_response("server is not ready"));
    }
    if let Some(readiness) = &server.readiness {
        let ready = call_server_closure(
            readiness,
            &[http_server_handle_value(server_id)],
            "http_server_request",
        )
        .await?;
        if !ready.is_truthy() {
            return Ok(unavailable_response("server is not ready"));
        }
    }

    let input = request.as_dict().cloned().unwrap_or_default();
    let method = input
        .get("method")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_string())
        .to_ascii_uppercase();
    let raw_path = input
        .get("path")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/".to_string());
    let (path, query) = split_path_and_query(&raw_path);
    let body_bytes = request_body_bytes(&input);

    let Some((route, path_params)) = matching_route(&server, &method, &path) else {
        return Ok(not_found_response(&method, &path));
    };

    let limit = route.max_body_bytes.unwrap_or(server.max_body_bytes);
    if body_bytes.len() > limit {
        return Ok(body_limit_response(limit, body_bytes.len()));
    }
    let retain_raw_body = route.retain_raw_body.unwrap_or(server.retain_raw_body);
    let mut req = request_value(
        &method,
        &path,
        path_params,
        query,
        &input,
        &body_bytes,
        retain_raw_body,
    );

    for before in &server.before {
        let result = call_server_closure(before, &[req.clone()], "http_server_request").await?;
        if value_is_response(&result) {
            return Ok(normalize_response(result));
        }
        if !matches!(result, VmValue::Nil) {
            req = result;
        }
    }

    let handler_result =
        call_server_closure(&route.handler, &[req.clone()], "http_server_request").await?;
    let mut response = normalize_response(handler_result);

    for after in &server.after {
        let result = call_server_closure(
            after,
            &[response.clone(), req.clone()],
            "http_server_request",
        )
        .await?;
        if !matches!(result, VmValue::Nil) {
            response = normalize_response(result);
        }
    }

    Ok(response)
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

    // --- Inbound HTTP server primitives ---

    vm.register_builtin("http_server", |args, _out| {
        let options = get_options_arg(args, 0);
        let server = HttpServer {
            routes: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            ready: get_bool_option(&options, "ready", true),
            readiness: None,
            shutdown_hooks: Vec::new(),
            shutdown: false,
            max_body_bytes: get_usize_option(
                &options,
                "max_body_bytes",
                DEFAULT_SERVER_MAX_BODY_BYTES,
            )?,
            retain_raw_body: get_bool_option(&options, "retain_raw_body", true),
        };
        let id = next_transport_handle("http-server");
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            if servers.len() >= MAX_HTTP_SERVERS {
                return Err(vm_error(format!(
                    "http_server: maximum open servers ({MAX_HTTP_SERVERS}) reached"
                )));
            }
            servers.insert(id.clone(), server);
            Ok(())
        })?;
        Ok(http_server_handle_value(&id))
    });

    vm.register_builtin("http_server_route", |args, _out| {
        if args.len() < 4 {
            return Err(vm_error(
                "http_server_route: requires server, method, path template, and handler",
            ));
        }
        let server_id = server_from_value(&args[0], "http_server_route")?;
        let method = args[1].display().to_ascii_uppercase();
        if method.is_empty() {
            return Err(vm_error("http_server_route: method is required"));
        }
        let template = args[2].display();
        if !template.starts_with('/') {
            return Err(vm_error(
                "http_server_route: path template must start with '/'",
            ));
        }
        let handler = closure_arg(args, 3, "http_server_route")?;
        let options = get_options_arg(args, 4);
        let route = HttpServerRoute {
            method,
            template,
            handler,
            max_body_bytes: get_optional_usize_option(&options, "max_body_bytes")?,
            retain_raw_body: match options.get("retain_raw_body") {
                Some(VmValue::Bool(value)) => Some(*value),
                _ => None,
            },
        };
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!("http_server_route: unknown server '{server_id}'"))
            })?;
            server.routes.push(route);
            Ok::<_, VmError>(())
        })?;
        Ok(http_server_handle_value(&server_id))
    });

    vm.register_builtin("http_server_before", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error("http_server_before: requires server and handler"));
        }
        let server_id = server_from_value(&args[0], "http_server_before")?;
        let handler = closure_arg(args, 1, "http_server_before")?;
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!("http_server_before: unknown server '{server_id}'"))
            })?;
            server.before.push(handler);
            Ok::<_, VmError>(())
        })?;
        Ok(http_server_handle_value(&server_id))
    });

    vm.register_builtin("http_server_after", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error("http_server_after: requires server and handler"));
        }
        let server_id = server_from_value(&args[0], "http_server_after")?;
        let handler = closure_arg(args, 1, "http_server_after")?;
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!("http_server_after: unknown server '{server_id}'"))
            })?;
            server.after.push(handler);
            Ok::<_, VmError>(())
        })?;
        Ok(http_server_handle_value(&server_id))
    });

    vm.register_async_builtin("http_server_request", |args| async move {
        if args.len() < 2 {
            return Err(vm_error("http_server_request: requires server and request"));
        }
        let server_id = server_from_value(&args[0], "http_server_request")?;
        run_http_server_request(&server_id, args[1].clone()).await
    });

    vm.register_async_builtin("http_server_test", |args| async move {
        if args.len() < 2 {
            return Err(vm_error("http_server_test: requires server and request"));
        }
        let server_id = server_from_value(&args[0], "http_server_test")?;
        run_http_server_request(&server_id, args[1].clone()).await
    });

    vm.register_builtin("http_server_set_ready", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error(
                "http_server_set_ready: requires server and ready bool",
            ));
        }
        let server_id = server_from_value(&args[0], "http_server_set_ready")?;
        let ready = matches!(args[1], VmValue::Bool(true));
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!(
                    "http_server_set_ready: unknown server '{server_id}'"
                ))
            })?;
            server.ready = ready;
            Ok::<_, VmError>(())
        })?;
        Ok(VmValue::Bool(ready))
    });

    vm.register_builtin("http_server_readiness", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error(
                "http_server_readiness: requires server and readiness closure",
            ));
        }
        let server_id = server_from_value(&args[0], "http_server_readiness")?;
        let handler = closure_arg(args, 1, "http_server_readiness")?;
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!(
                    "http_server_readiness: unknown server '{server_id}'"
                ))
            })?;
            server.readiness = Some(handler);
            Ok::<_, VmError>(())
        })?;
        Ok(http_server_handle_value(&server_id))
    });

    vm.register_async_builtin("http_server_ready", |args| async move {
        let Some(server_arg) = args.first() else {
            return Err(vm_error("http_server_ready: requires server"));
        };
        let server_id = server_from_value(server_arg, "http_server_ready")?;
        let server = HTTP_SERVERS.with(|servers| servers.borrow().get(&server_id).cloned());
        let Some(server) = server else {
            return Err(vm_error(format!(
                "http_server_ready: unknown server '{server_id}'"
            )));
        };
        if server.shutdown {
            return Ok(VmValue::Bool(false));
        }
        let Some(readiness) = server.readiness else {
            return Ok(VmValue::Bool(server.ready));
        };
        let result = call_server_closure(
            &readiness,
            &[http_server_handle_value(&server_id)],
            "http_server_ready",
        )
        .await?;
        Ok(VmValue::Bool(result.is_truthy()))
    });

    vm.register_builtin("http_server_on_shutdown", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error(
                "http_server_on_shutdown: requires server and handler",
            ));
        }
        let server_id = server_from_value(&args[0], "http_server_on_shutdown")?;
        let handler = closure_arg(args, 1, "http_server_on_shutdown")?;
        HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!(
                    "http_server_on_shutdown: unknown server '{server_id}'"
                ))
            })?;
            server.shutdown_hooks.push(handler);
            Ok::<_, VmError>(())
        })?;
        Ok(http_server_handle_value(&server_id))
    });

    vm.register_async_builtin("http_server_shutdown", |args| async move {
        let Some(server_arg) = args.first() else {
            return Err(vm_error("http_server_shutdown: requires server"));
        };
        let server_id = server_from_value(server_arg, "http_server_shutdown")?;
        let hooks = HTTP_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            let server = servers.get_mut(&server_id).ok_or_else(|| {
                vm_error(format!(
                    "http_server_shutdown: unknown server '{server_id}'"
                ))
            })?;
            server.shutdown = true;
            Ok::<_, VmError>(server.shutdown_hooks.clone())
        })?;
        for hook in hooks {
            let _ = call_server_closure(
                &hook,
                &[http_server_handle_value(&server_id)],
                "http_server_shutdown",
            )
            .await?;
        }
        Ok(VmValue::Bool(true))
    });

    vm.register_builtin("http_response", |args, _out| {
        let status = args.first().and_then(VmValue::as_int).unwrap_or(200);
        let body = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let headers = args
            .get(2)
            .and_then(VmValue::as_dict)
            .cloned()
            .unwrap_or_default();
        Ok(response_with_kind(status, headers, body, "text"))
    });

    vm.register_builtin("http_response_text", |args, _out| {
        let body = args.first().cloned().unwrap_or(VmValue::Nil);
        let options = get_options_arg(args, 1);
        let status = options
            .get("status")
            .and_then(VmValue::as_int)
            .unwrap_or(200);
        let headers = options
            .get("headers")
            .and_then(VmValue::as_dict)
            .cloned()
            .unwrap_or_default();
        Ok(response_with_kind(status, headers, body, "text"))
    });

    vm.register_builtin("http_response_json", |args, _out| {
        let body = args
            .first()
            .map(crate::stdlib::json::vm_value_to_json)
            .map(vm_string)
            .unwrap_or_else(|| vm_string("null"));
        let options = get_options_arg(args, 1);
        let status = options
            .get("status")
            .and_then(VmValue::as_int)
            .unwrap_or(200);
        let headers = options
            .get("headers")
            .and_then(VmValue::as_dict)
            .cloned()
            .unwrap_or_default();
        Ok(response_with_kind(status, headers, body, "json"))
    });

    vm.register_builtin("http_response_bytes", |args, _out| {
        let body = match args.first() {
            Some(VmValue::Bytes(bytes)) => VmValue::Bytes(bytes.clone()),
            Some(value) => VmValue::Bytes(Rc::new(value.display().into_bytes())),
            None => VmValue::Bytes(Rc::new(Vec::new())),
        };
        let options = get_options_arg(args, 1);
        let status = options
            .get("status")
            .and_then(VmValue::as_int)
            .unwrap_or(200);
        let headers = options
            .get("headers")
            .and_then(VmValue::as_dict)
            .cloned()
            .unwrap_or_default();
        Ok(response_with_kind(status, headers, body, "bytes"))
    });

    vm.register_builtin("http_header", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error(
                "http_header: requires headers/request/response and name",
            ));
        }
        let headers = headers_from_value(&args[0]);
        Ok(header_lookup_value(&headers, &args[1].display()))
    });

    // --- Mock HTTP builtins ---

    // http_mock(method, url_pattern, response) -> nil
    //
    // Calling http_mock again with the same (method, url_pattern) tuple
    // *replaces* the prior mock for that target — tests can override a
    // per-case response without first calling http_mock_clear().
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
            let mut mocks = mocks.borrow_mut();
            mocks.retain(|mock| !(mock.method == method && mock.url_pattern == url_pattern));
            mocks.push(HttpMock {
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
        HTTP_STREAMS.with(|streams| streams.borrow_mut().clear());
        Ok(VmValue::Nil)
    });

    // http_mock_calls() -> list of {method, url, headers, body}
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
                    "headers".to_string(),
                    VmValue::Dict(Rc::new(c.headers.clone())),
                );
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

    vm.register_async_builtin("http_download", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(vm_error("http_download: URL is required"));
        }
        let dst_path = args.get(1).map(|a| a.display()).unwrap_or_default();
        if dst_path.is_empty() {
            return Err(vm_error("http_download: destination path is required"));
        }
        let options = get_options_arg(&args, 2);
        vm_http_download(&url, &dst_path, &options).await
    });

    vm.register_async_builtin("http_stream_open", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(vm_error("http_stream_open: URL is required"));
        }
        let options = get_options_arg(&args, 1);
        vm_http_stream_open(&url, &options).await
    });

    vm.register_async_builtin("http_stream_read", |args| async move {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_stream_read: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "http_stream_read")?;
        let max_bytes = args
            .get(1)
            .and_then(|value| value.as_int())
            .map(|value| value.max(1) as usize)
            .unwrap_or(DEFAULT_MAX_MESSAGE_BYTES);
        vm_http_stream_read(&stream_id, max_bytes).await
    });

    vm.register_builtin("http_stream_info", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_stream_info: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "http_stream_info")?;
        vm_http_stream_info(&stream_id)
    });

    vm.register_builtin("http_stream_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_stream_close: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "http_stream_close")?;
        let removed = HTTP_STREAMS.with(|streams| streams.borrow_mut().remove(&stream_id));
        Ok(VmValue::Bool(removed.is_some()))
    });

    vm.register_builtin("http_session", |args, _out| {
        let options = get_options_arg(args, 0);
        let config = parse_http_options(&options);
        let client = build_http_client(&config)?;
        let id = next_transport_handle("http-session");
        HTTP_SESSIONS.with(|sessions| {
            let mut sessions = sessions.borrow_mut();
            if sessions.len() >= MAX_HTTP_SESSIONS {
                return Err(vm_error(format!(
                    "http_session: maximum open sessions ({MAX_HTTP_SESSIONS}) reached"
                )));
            }
            sessions.insert(id.clone(), HttpSession { client, options });
            Ok(())
        })?;
        Ok(VmValue::String(Rc::from(id)))
    });

    vm.register_async_builtin("http_session_request", |args| async move {
        if args.len() < 3 {
            return Err(vm_error(
                "http_session_request: requires session, method, and URL",
            ));
        }
        let session_id = handle_from_value(&args[0], "http_session_request")?;
        let method = args[1].display().to_uppercase();
        if method.is_empty() {
            return Err(vm_error("http_session_request: method is required"));
        }
        let url = args[2].display();
        if url.is_empty() {
            return Err(vm_error("http_session_request: URL is required"));
        }
        let options = get_options_arg(&args, 3);
        vm_execute_http_session_request(&session_id, &method, &url, &options).await
    });

    vm.register_builtin("http_session_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_session_close: requires a session handle"));
        };
        let session_id = handle_from_value(handle, "http_session_close")?;
        let removed = HTTP_SESSIONS.with(|sessions| sessions.borrow_mut().remove(&session_id));
        Ok(VmValue::Bool(removed.is_some()))
    });

    vm.register_builtin("sse_mock", |args, _out| {
        let url_pattern = args.first().map(|arg| arg.display()).unwrap_or_default();
        if url_pattern.is_empty() {
            return Err(vm_error("sse_mock: URL pattern is required"));
        }
        let events = parse_mock_stream_events(args.get(1));
        SSE_MOCKS.with(|mocks| {
            mocks.borrow_mut().push(SseMock {
                url_pattern,
                events,
            });
        });
        Ok(VmValue::Nil)
    });

    vm.register_async_builtin("sse_connect", |args| async move {
        let method = args
            .first()
            .map(|arg| arg.display())
            .filter(|method| !method.is_empty())
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let url = args.get(1).map(|arg| arg.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(vm_error("sse_connect: URL is required"));
        }
        let options = get_options_arg(&args, 2);
        vm_sse_connect(&method, &url, &options).await
    });

    vm.register_async_builtin("sse_receive", |args| async move {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_receive: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_receive")?;
        let timeout_ms = receive_timeout_arg(&args, 1);
        vm_sse_receive(&stream_id, timeout_ms).await
    });

    vm.register_builtin("sse_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_close: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_close")?;
        let removed = SSE_HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            let removed = handles.remove(&stream_id);
            if let Some(handle) = &removed {
                if let SseHandleKind::Real(stream) = &handle.kind {
                    if let Ok(mut stream) = stream.try_lock() {
                        stream.close();
                    }
                }
            }
            removed
        });
        Ok(VmValue::Bool(removed.is_some()))
    });

    vm.register_builtin("websocket_mock", |args, _out| {
        let url_pattern = args.first().map(|arg| arg.display()).unwrap_or_default();
        if url_pattern.is_empty() {
            return Err(vm_error("websocket_mock: URL pattern is required"));
        }
        let (messages, echo) = parse_websocket_mock(args.get(1));
        WEBSOCKET_MOCKS.with(|mocks| {
            mocks.borrow_mut().push(WebSocketMock {
                url_pattern,
                messages,
                echo,
            });
        });
        Ok(VmValue::Nil)
    });

    vm.register_async_builtin("websocket_connect", |args| async move {
        let url = args.first().map(|arg| arg.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(vm_error("websocket_connect: URL is required"));
        }
        let options = get_options_arg(&args, 1);
        vm_websocket_connect(&url, &options).await
    });

    vm.register_async_builtin("websocket_send", |args| async move {
        if args.len() < 2 {
            return Err(vm_error(
                "websocket_send: requires socket handle and message",
            ));
        }
        let socket_id = handle_from_value(&args[0], "websocket_send")?;
        let message = args[1].clone();
        let options = get_options_arg(&args, 2);
        vm_websocket_send(&socket_id, message, &options).await
    });

    vm.register_async_builtin("websocket_receive", |args| async move {
        let Some(handle) = args.first() else {
            return Err(vm_error("websocket_receive: requires a socket handle"));
        };
        let socket_id = handle_from_value(handle, "websocket_receive")?;
        let timeout_ms = receive_timeout_arg(&args, 1);
        vm_websocket_receive(&socket_id, timeout_ms).await
    });

    vm.register_async_builtin("websocket_close", |args| async move {
        let Some(handle) = args.first() else {
            return Err(vm_error("websocket_close: requires a socket handle"));
        };
        let socket_id = handle_from_value(handle, "websocket_close")?;
        vm_websocket_close(&socket_id).await
    });

    vm.register_builtin("transport_mock_clear", |_args, _out| {
        HTTP_STREAMS.with(|streams| streams.borrow_mut().clear());
        SSE_MOCKS.with(|mocks| mocks.borrow_mut().clear());
        SSE_HANDLES.with(|handles| handles.borrow_mut().clear());
        WEBSOCKET_MOCKS.with(|mocks| mocks.borrow_mut().clear());
        WEBSOCKET_HANDLES.with(|handles| handles.borrow_mut().clear());
        TRANSPORT_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
        Ok(VmValue::Nil)
    });

    vm.register_builtin("transport_mock_calls", |_args, _out| {
        let calls = TRANSPORT_MOCK_CALLS.with(|calls| calls.borrow().clone());
        let values = calls
            .iter()
            .map(transport_mock_call_value)
            .collect::<Vec<_>>();
        Ok(VmValue::List(Rc::new(values)))
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

fn vm_get_optional_int_option(options: &BTreeMap<String, VmValue>, key: &str) -> Option<u64> {
    options
        .get(key)
        .and_then(|value| value.as_int())
        .map(|value| value.max(0) as u64)
}

fn string_option(options: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    options
        .get(key)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
}

fn parse_proxy_config(options: &BTreeMap<String, VmValue>) -> Option<HttpProxyConfig> {
    let proxy = options.get("proxy")?;
    let (url, no_proxy) = match proxy {
        VmValue::Dict(dict) => (
            dict.get("url")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())?,
            dict.get("no_proxy")
                .map(|value| value.display())
                .filter(|value| !value.is_empty()),
        ),
        other => (other.display(), None),
    };
    if url.is_empty() {
        return None;
    }
    let auth = options
        .get("proxy_auth")
        .and_then(|value| value.as_dict())
        .map(|dict| {
            (
                dict.get("user")
                    .map(|value| value.display())
                    .unwrap_or_default(),
                dict.get("pass")
                    .or_else(|| dict.get("password"))
                    .map(|value| value.display())
                    .unwrap_or_default(),
            )
        })
        .filter(|(user, _)| !user.is_empty());
    Some(HttpProxyConfig {
        url,
        auth,
        no_proxy,
    })
}

fn parse_tls_config(options: &BTreeMap<String, VmValue>) -> HttpTlsConfig {
    let Some(tls) = options.get("tls").and_then(|value| value.as_dict()) else {
        return HttpTlsConfig::default();
    };
    let pinned_sha256 = match tls.get("pinned_sha256") {
        Some(VmValue::List(values)) => values
            .iter()
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .collect(),
        Some(value) => {
            let value = value.display();
            if value.is_empty() {
                Vec::new()
            } else {
                vec![value]
            }
        }
        None => Vec::new(),
    };
    HttpTlsConfig {
        ca_bundle_path: string_option(tls, "ca_bundle_path"),
        client_cert_path: string_option(tls, "client_cert_path"),
        client_key_path: string_option(tls, "client_key_path"),
        client_identity_path: string_option(tls, "client_identity_path"),
        pinned_sha256,
    }
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
    let total_timeout_ms = vm_get_int_option(options, "total_timeout_ms", -1);
    let total_timeout_ms = if total_timeout_ms >= 0 {
        total_timeout_ms as u64
    } else {
        vm_get_int_option_prefer(options, "timeout_ms", "timeout", DEFAULT_TIMEOUT_MS as i64).max(0)
            as u64
    };
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
        total_timeout_ms,
        connect_timeout_ms: vm_get_optional_int_option(options, "connect_timeout_ms"),
        read_timeout_ms: vm_get_optional_int_option(options, "read_timeout_ms"),
        retry: RetryConfig {
            max: retry_max,
            backoff_ms: retry_backoff_ms,
            retryable_statuses: parse_retry_statuses(options),
            retryable_methods: parse_retry_methods(options),
            respect_retry_after,
        },
        follow_redirects,
        max_redirects,
        proxy: parse_proxy_config(options),
        tls: parse_tls_config(options),
        decompress: vm_get_bool_option(options, "decompress", true),
    }
}

fn http_client_key(config: &HttpRequestConfig) -> String {
    format!(
        "follow_redirects={};max_redirects={};connect_timeout={:?};read_timeout={:?};proxy={};proxy_auth={};no_proxy={};ca={};client_cert={};client_key={};identity={};pins={};decompress={}",
        config.follow_redirects,
        config.max_redirects,
        config.connect_timeout_ms,
        config.read_timeout_ms,
        config
            .proxy
            .as_ref()
            .map(|proxy| proxy.url.as_str())
            .unwrap_or(""),
        config
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.auth.as_ref())
            .map(|(user, _)| user.as_str())
            .unwrap_or(""),
        config
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.no_proxy.as_deref())
            .unwrap_or(""),
        config.tls.ca_bundle_path.as_deref().unwrap_or(""),
        config.tls.client_cert_path.as_deref().unwrap_or(""),
        config.tls.client_key_path.as_deref().unwrap_or(""),
        config.tls.client_identity_path.as_deref().unwrap_or(""),
        config.tls.pinned_sha256.join(","),
        config.decompress,
    )
}

fn build_http_client(config: &HttpRequestConfig) -> Result<reqwest::Client, VmError> {
    let redirect_policy = if config.follow_redirects {
        reqwest::redirect::Policy::limited(config.max_redirects)
    } else {
        reqwest::redirect::Policy::none()
    };

    let mut builder = reqwest::Client::builder().redirect(redirect_policy);
    if let Some(ms) = config.connect_timeout_ms {
        builder = builder.connect_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = config.read_timeout_ms {
        builder = builder.read_timeout(Duration::from_millis(ms));
    }
    if !config.decompress {
        builder = builder.no_gzip().no_brotli().no_deflate().no_zstd();
    }
    if let Some(proxy_config) = &config.proxy {
        let mut proxy = reqwest::Proxy::all(&proxy_config.url)
            .map_err(|e| vm_error(format!("http: invalid proxy '{}': {e}", proxy_config.url)))?;
        if let Some((user, pass)) = &proxy_config.auth {
            proxy = proxy.basic_auth(user, pass);
        }
        if let Some(no_proxy) = &proxy_config.no_proxy {
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy));
        }
        builder = builder.proxy(proxy);
    }
    builder = configure_tls(builder, &config.tls)?;
    builder
        .build()
        .map_err(|e| vm_error(format!("http: failed to build client: {e}")))
}

fn configure_tls(
    mut builder: reqwest::ClientBuilder,
    tls: &HttpTlsConfig,
) -> Result<reqwest::ClientBuilder, VmError> {
    if let Some(path) = &tls.ca_bundle_path {
        let resolved = resolve_http_path("http tls", path, crate::stdlib::sandbox::FsAccess::Read)?;
        let bytes = std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read CA bundle {}: {error}",
                resolved.display()
            ))
        })?;
        match reqwest::Certificate::from_pem_bundle(&bytes) {
            Ok(certs) => {
                for cert in certs {
                    builder = builder.add_root_certificate(cert);
                }
            }
            Err(pem_error) => {
                let cert = reqwest::Certificate::from_der(&bytes).map_err(|der_error| {
                    vm_error(format!(
                        "http: failed to parse CA bundle {} as PEM ({pem_error}) or DER ({der_error})",
                        resolved.display()
                    ))
                })?;
                builder = builder.add_root_certificate(cert);
            }
        }
    }

    if let Some(path) = &tls.client_identity_path {
        let resolved = resolve_http_path("http tls", path, crate::stdlib::sandbox::FsAccess::Read)?;
        let bytes = std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read client identity {}: {error}",
                resolved.display()
            ))
        })?;
        let identity = reqwest::Identity::from_pem(&bytes).map_err(|error| {
            vm_error(format!(
                "http: failed to parse client identity {}: {error}",
                resolved.display()
            ))
        })?;
        builder = builder.identity(identity);
    } else if let Some(cert_path) = &tls.client_cert_path {
        let cert = {
            let resolved = resolve_http_path(
                "http tls",
                cert_path,
                crate::stdlib::sandbox::FsAccess::Read,
            )?;
            std::fs::read(&resolved).map_err(|error| {
                vm_error(format!(
                    "http: failed to read client certificate {}: {error}",
                    resolved.display()
                ))
            })?
        };
        let mut identity_pem = cert;
        if let Some(key_path) = &tls.client_key_path {
            let resolved =
                resolve_http_path("http tls", key_path, crate::stdlib::sandbox::FsAccess::Read)?;
            let key = std::fs::read(&resolved).map_err(|error| {
                vm_error(format!(
                    "http: failed to read client key {}: {error}",
                    resolved.display()
                ))
            })?;
            identity_pem.extend_from_slice(b"\n");
            identity_pem.extend_from_slice(&key);
        }
        let identity = reqwest::Identity::from_pem(&identity_pem)
            .map_err(|error| vm_error(format!("http: failed to parse client identity: {error}")))?;
        builder = builder.identity(identity);
    }

    if !tls.pinned_sha256.is_empty() {
        builder = builder.tls_info(true);
    }
    Ok(builder)
}

fn pooled_http_client(config: &HttpRequestConfig) -> Result<reqwest::Client, VmError> {
    let key = http_client_key(config);
    if let Some(client) = HTTP_CLIENTS.with(|clients| clients.borrow().get(&key).cloned()) {
        return Ok(client);
    }

    let client = build_http_client(config)?;
    HTTP_CLIENTS.with(|clients| {
        clients.borrow_mut().insert(key, client.clone());
    });
    Ok(client)
}

fn normalize_pin(value: &str) -> String {
    let trimmed = value.trim();
    let trimmed = trimmed
        .strip_prefix("sha256/")
        .or_else(|| trimmed.strip_prefix("sha256:"))
        .unwrap_or(trimmed);
    let compact = trimmed.replace(':', "");
    if !compact.is_empty() && compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        compact.to_ascii_lowercase()
    } else {
        compact
    }
}

fn verify_tls_pin(response: &reqwest::Response, pins: &[String]) -> Result<(), VmError> {
    if pins.is_empty() {
        return Ok(());
    }
    let Some(info) = response.extensions().get::<reqwest::tls::TlsInfo>() else {
        return Err(vm_error(
            "http: TLS pinning requested but TLS info is unavailable",
        ));
    };
    let Some(cert_der) = info.peer_certificate() else {
        return Err(vm_error(
            "http: TLS pinning requested but no peer certificate was presented",
        ));
    };
    let (_, cert) = X509Certificate::from_der(cert_der)
        .map_err(|error| vm_error(format!("http: failed to parse peer certificate: {error}")))?;
    let digest = Sha256::digest(cert.tbs_certificate.subject_pki.raw);
    let hex_pin = hex::encode(digest.as_slice());
    let base64_pin = base64::engine::general_purpose::STANDARD.encode(digest);
    let base64url_pin = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let wanted = pins
        .iter()
        .map(|pin| normalize_pin(pin))
        .collect::<Vec<_>>();
    if wanted
        .iter()
        .any(|pin| pin == &hex_pin || pin == &base64_pin || pin == &base64url_pin)
    {
        Ok(())
    } else {
        Err(vm_error("http: TLS SPKI pin mismatch"))
    }
}

fn parse_http_request_parts(
    method: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<HttpRequestParts, VmError> {
    let req_method = method
        .parse::<reqwest::Method>()
        .map_err(|e| vm_error(format!("http: invalid method '{method}': {e}")))?;

    let mut header_map = reqwest::header::HeaderMap::new();
    let mut recorded_headers = BTreeMap::new();

    if let Some(auth_val) = options.get("auth") {
        match auth_val {
            VmValue::String(s) => {
                let hv = reqwest::header::HeaderValue::from_str(s)
                    .map_err(|e| vm_error(format!("http: invalid auth header value: {e}")))?;
                header_map.insert(reqwest::header::AUTHORIZATION, hv);
                recorded_headers.insert(
                    "Authorization".to_string(),
                    VmValue::String(Rc::from(s.as_ref())),
                );
            }
            VmValue::Dict(d) => {
                if let Some(bearer) = d.get("bearer") {
                    let token = bearer.display();
                    let authorization = format!("Bearer {token}");
                    let hv = reqwest::header::HeaderValue::from_str(&authorization)
                        .map_err(|e| vm_error(format!("http: invalid bearer token: {e}")))?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                    recorded_headers.insert(
                        "Authorization".to_string(),
                        VmValue::String(Rc::from(authorization)),
                    );
                } else if let Some(VmValue::Dict(basic)) = d.get("basic") {
                    let user = basic.get("user").map(|v| v.display()).unwrap_or_default();
                    let password = basic
                        .get("password")
                        .map(|v| v.display())
                        .unwrap_or_default();
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{user}:{password}"));
                    let authorization = format!("Basic {encoded}");
                    let hv = reqwest::header::HeaderValue::from_str(&authorization)
                        .map_err(|e| vm_error(format!("http: invalid basic auth: {e}")))?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                    recorded_headers.insert(
                        "Authorization".to_string(),
                        VmValue::String(Rc::from(authorization)),
                    );
                }
            }
            _ => {}
        }
    }

    if let Some(VmValue::Dict(hdrs)) = options.get("headers") {
        for (k, v) in hdrs.iter() {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| vm_error(format!("http: invalid header name '{k}': {e}")))?;
            let val = reqwest::header::HeaderValue::from_str(&v.display())
                .map_err(|e| vm_error(format!("http: invalid header value for '{k}': {e}")))?;
            header_map.insert(name, val);
            recorded_headers.insert(k.clone(), VmValue::String(Rc::from(v.display())));
        }
    }

    let multipart = parse_multipart_request(options)?;
    if multipart.is_some() {
        if options.contains_key("body") {
            return Err(vm_error(
                "http: body and multipart options are mutually exclusive",
            ));
        }
        recorded_headers.insert(
            "content-type".to_string(),
            VmValue::String(Rc::from(format!(
                "multipart/form-data; boundary={MULTIPART_MOCK_BOUNDARY}"
            ))),
        );
    }

    Ok(HttpRequestParts {
        method: req_method,
        headers: header_map,
        recorded_headers,
        body: if multipart.is_some() {
            multipart.as_ref().map(|request| request.mock_body.clone())
        } else {
            options.get("body").map(|v| v.display())
        },
        multipart,
    })
}

fn session_from_options(options: &BTreeMap<String, VmValue>) -> Option<String> {
    options
        .get("session")
        .and_then(|value| handle_from_value(value, "http_request").ok())
}

fn parse_mock_stream_event(value: &VmValue) -> MockStreamEvent {
    match value {
        VmValue::Dict(dict) => MockStreamEvent {
            event_type: dict
                .get("event")
                .or_else(|| dict.get("type"))
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "message".to_string()),
            data: dict
                .get("data")
                .map(|value| value.display())
                .unwrap_or_default(),
            id: dict
                .get("id")
                .map(|value| value.display())
                .filter(|value| !value.is_empty()),
            retry_ms: dict.get("retry_ms").and_then(|value| value.as_int()),
        },
        _ => MockStreamEvent {
            event_type: "message".to_string(),
            data: value.display(),
            id: None,
            retry_ms: None,
        },
    }
}

fn parse_mock_stream_events(value: Option<&VmValue>) -> Vec<MockStreamEvent> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        VmValue::Dict(dict) => dict
            .get("events")
            .and_then(|events| match events {
                VmValue::List(items) => Some(items.iter().map(parse_mock_stream_event).collect()),
                _ => None,
            })
            .unwrap_or_default(),
        VmValue::List(items) => items.iter().map(parse_mock_stream_event).collect(),
        other => vec![parse_mock_stream_event(other)],
    }
}

fn sse_event_value(event: &MockStreamEvent) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("type".to_string(), VmValue::String(Rc::from("event")));
    dict.insert(
        "event".to_string(),
        VmValue::String(Rc::from(event.event_type.as_str())),
    );
    dict.insert(
        "data".to_string(),
        VmValue::String(Rc::from(event.data.as_str())),
    );
    dict.insert(
        "id".to_string(),
        event
            .id
            .as_deref()
            .map(|id| VmValue::String(Rc::from(id)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "retry_ms".to_string(),
        event.retry_ms.map(VmValue::Int).unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
}

fn real_sse_event_value(event: SseEvent) -> VmValue {
    match event {
        SseEvent::Open => {
            let mut dict = BTreeMap::new();
            dict.insert("type".to_string(), VmValue::String(Rc::from("open")));
            VmValue::Dict(Rc::new(dict))
        }
        SseEvent::Message(message) => {
            let retry_ms = message.retry.map(|retry| retry.as_millis() as i64);
            sse_event_value(&MockStreamEvent {
                event_type: if message.event.is_empty() {
                    "message".to_string()
                } else {
                    message.event
                },
                data: message.data,
                id: if message.id.is_empty() {
                    None
                } else {
                    Some(message.id)
                },
                retry_ms,
            })
        }
    }
}

fn consume_sse_mock(url: &str) -> Option<Vec<MockStreamEvent>> {
    SSE_MOCKS.with(|mocks| {
        mocks
            .borrow()
            .iter()
            .find(|mock| url_matches(&mock.url_pattern, url))
            .map(|mock| mock.events.clone())
    })
}

fn parse_ws_message(value: &VmValue) -> MockWsMessage {
    match value {
        VmValue::Dict(dict) => {
            let message_type = dict
                .get("type")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "text".to_string());
            let data = if dict
                .get("base64")
                .and_then(|value| match value {
                    VmValue::Bool(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(false)
            {
                use base64::Engine;
                dict.get("data")
                    .map(|value| value.display())
                    .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
                    .unwrap_or_default()
            } else {
                dict.get("data")
                    .map(|value| value.display().into_bytes())
                    .unwrap_or_default()
            };
            MockWsMessage { message_type, data }
        }
        VmValue::Bytes(bytes) => MockWsMessage {
            message_type: "binary".to_string(),
            data: bytes.as_ref().clone(),
        },
        other => MockWsMessage {
            message_type: "text".to_string(),
            data: other.display().into_bytes(),
        },
    }
}

fn parse_websocket_mock(value: Option<&VmValue>) -> (Vec<MockWsMessage>, bool) {
    let Some(value) = value else {
        return (Vec::new(), false);
    };
    match value {
        VmValue::Dict(dict) => {
            let echo = dict
                .get("echo")
                .and_then(|value| match value {
                    VmValue::Bool(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(false);
            let messages = dict
                .get("messages")
                .and_then(|messages| match messages {
                    VmValue::List(items) => Some(items.iter().map(parse_ws_message).collect()),
                    _ => None,
                })
                .unwrap_or_default();
            (messages, echo)
        }
        VmValue::List(items) => (items.iter().map(parse_ws_message).collect(), false),
        other => (vec![parse_ws_message(other)], false),
    }
}

fn consume_websocket_mock(url: &str) -> Option<(Vec<MockWsMessage>, bool)> {
    WEBSOCKET_MOCKS.with(|mocks| {
        mocks
            .borrow()
            .iter()
            .find(|mock| url_matches(&mock.url_pattern, url))
            .map(|mock| (mock.messages.clone(), mock.echo))
    })
}

fn ws_message_data(message: &MockWsMessage) -> String {
    match message.message_type.as_str() {
        "text" => String::from_utf8_lossy(&message.data).into_owned(),
        _ => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&message.data)
        }
    }
}

fn ws_event_value(message: MockWsMessage) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "type".to_string(),
        VmValue::String(Rc::from(message.message_type.as_str())),
    );
    match message.message_type.as_str() {
        "text" => {
            dict.insert(
                "data".to_string(),
                VmValue::String(Rc::from(String::from_utf8_lossy(&message.data).as_ref())),
            );
        }
        _ => {
            use base64::Engine;
            dict.insert(
                "data_base64".to_string(),
                VmValue::String(Rc::from(
                    base64::engine::general_purpose::STANDARD
                        .encode(&message.data)
                        .as_str(),
                )),
            );
        }
    }
    VmValue::Dict(Rc::new(dict))
}

fn real_ws_event_value(message: WsMessage) -> VmValue {
    match message {
        WsMessage::Text(text) => ws_event_value(MockWsMessage {
            message_type: "text".to_string(),
            data: text.as_bytes().to_vec(),
        }),
        WsMessage::Binary(bytes) => ws_event_value(MockWsMessage {
            message_type: "binary".to_string(),
            data: bytes.to_vec(),
        }),
        WsMessage::Ping(bytes) => ws_event_value(MockWsMessage {
            message_type: "ping".to_string(),
            data: bytes.to_vec(),
        }),
        WsMessage::Pong(bytes) => ws_event_value(MockWsMessage {
            message_type: "pong".to_string(),
            data: bytes.to_vec(),
        }),
        WsMessage::Close(_) => closed_event(),
        WsMessage::Frame(_) => VmValue::Nil,
    }
}

fn transport_mock_call_value(call: &TransportMockCall) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(call.kind.as_str())),
    );
    dict.insert(
        "url".to_string(),
        VmValue::String(Rc::from(call.url.as_str())),
    );
    dict.insert(
        "handle".to_string(),
        call.handle
            .as_deref()
            .map(|handle| VmValue::String(Rc::from(handle)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "type".to_string(),
        call.message_type
            .as_deref()
            .map(|message_type| VmValue::String(Rc::from(message_type)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "data".to_string(),
        call.data
            .as_deref()
            .map(|data| VmValue::String(Rc::from(data)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
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
    if let Some(session_id) = session_from_options(options) {
        return vm_execute_http_session_request(&session_id, method, url, options).await;
    }

    let config = parse_http_options(options);
    let client = pooled_http_client(&config)?;
    vm_execute_http_request_with_client(client, &config, method, url, options).await
}

async fn vm_execute_http_session_request(
    session_id: &str,
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let session = HTTP_SESSIONS.with(|sessions| sessions.borrow().get(session_id).cloned());
    let Some(session) = session else {
        return Err(vm_error(format!(
            "http_session_request: unknown HTTP session '{session_id}'"
        )));
    };
    let merged_options = merge_options(&session.options, options);
    let config = parse_http_options(&merged_options);
    vm_execute_http_request_with_client(session.client, &config, method, url, &merged_options).await
}

async fn vm_execute_http_request_with_client(
    client: reqwest::Client,
    config: &HttpRequestConfig,
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let parts = parse_http_request_parts(method, options)?;

    for attempt in 0..=config.retry.max {
        if let Some(mock_response) = consume_http_mock(
            method,
            url,
            parts.recorded_headers.clone(),
            parts.body.clone(),
        ) {
            let status = mock_response.status.clamp(0, u16::MAX as i64) as u16;
            if should_retry_response(config, &parts.method, status, attempt) {
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
            return Err(vm_error(format!(
                "http: URL must start with http:// or https://, got '{url}'"
            )));
        }

        let mut req = client.request(parts.method.clone(), url);
        req = req
            .headers(parts.headers.clone())
            .timeout(Duration::from_millis(config.total_timeout_ms));
        if let Some(multipart) = &parts.multipart {
            req = req.multipart(multipart_form(multipart)?);
        } else if let Some(ref b) = parts.body {
            req = req.body(b.clone());
        }

        match req.send().await {
            Ok(response) => {
                verify_tls_pin(&response, &config.tls.pinned_sha256)?;
                let status = response.status().as_u16();
                if should_retry_response(config, &parts.method, status, attempt) {
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

                let resp_headers = response_headers(response.headers());

                let body_text = response
                    .text()
                    .await
                    .map_err(|e| vm_error(format!("http: failed to read response body: {e}")))?;
                return Ok(build_http_response(status as i64, resp_headers, body_text));
            }
            Err(e) => {
                if should_retry_transport(config, &parts.method, &e, attempt) {
                    tokio::time::sleep(compute_retry_delay(attempt, config.retry.backoff_ms, None))
                        .await;
                    continue;
                }
                return Err(vm_error(format!("http: request failed: {e}")));
            }
        }
    }

    Err(vm_error("http: request failed"))
}

async fn vm_http_download(
    url: &str,
    dst_path: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let method = options
        .get("method")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_string())
        .to_uppercase();
    let parts = parse_http_request_parts(&method, options)?;
    if let Some(mock_response) = consume_http_mock(
        &method,
        url,
        parts.recorded_headers.clone(),
        parts.body.clone(),
    ) {
        let resolved = resolve_http_path(
            "http_download",
            dst_path,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        std::fs::write(&resolved, mock_response.body.as_bytes()).map_err(|error| {
            vm_error(format!(
                "http_download: failed to write {}: {error}",
                resolved.display()
            ))
        })?;
        return Ok(build_http_download_response(
            mock_response.status,
            mock_response.headers,
            mock_response.body.len() as u64,
        ));
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(vm_error(format!(
            "http_download: URL must start with http:// or https://, got '{url}'"
        )));
    }
    let config = parse_http_options(options);
    let client = if let Some(session_id) = session_from_options(options) {
        HTTP_SESSIONS
            .with(|sessions| sessions.borrow().get(&session_id).cloned())
            .map(|session| session.client)
            .ok_or_else(|| {
                vm_error(format!(
                    "http_download: unknown HTTP session '{session_id}'"
                ))
            })?
    } else {
        pooled_http_client(&config)?
    };
    let mut request = client
        .request(parts.method, url)
        .headers(parts.headers)
        .timeout(Duration::from_millis(config.total_timeout_ms));
    if let Some(multipart) = &parts.multipart {
        request = request.multipart(multipart_form(multipart)?);
    } else if let Some(body) = parts.body {
        request = request.body(body);
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| vm_error(format!("http_download: request failed: {error}")))?;
    verify_tls_pin(&response, &config.tls.pinned_sha256)?;
    let status = response.status().as_u16() as i64;
    let headers = response_headers(response.headers());
    let resolved = resolve_http_path(
        "http_download",
        dst_path,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    let mut file = std::fs::File::create(&resolved).map_err(|error| {
        vm_error(format!(
            "http_download: failed to create {}: {error}",
            resolved.display()
        ))
    })?;
    let mut bytes_written = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        vm_error(format!(
            "http_download: failed to read response body: {error}"
        ))
    })? {
        file.write_all(&chunk).map_err(|error| {
            vm_error(format!(
                "http_download: failed to write {}: {error}",
                resolved.display()
            ))
        })?;
        bytes_written += chunk.len() as u64;
    }
    Ok(build_http_download_response(status, headers, bytes_written))
}

async fn vm_http_stream_open(
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let method = options
        .get("method")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_string())
        .to_uppercase();
    let parts = parse_http_request_parts(&method, options)?;
    let id = next_transport_handle("http-stream");
    if let Some(mock_response) = consume_http_mock(
        &method,
        url,
        parts.recorded_headers.clone(),
        parts.body.clone(),
    ) {
        let handle = HttpStreamHandle {
            kind: HttpStreamKind::Fake,
            status: mock_response.status,
            headers: mock_response.headers,
            pending: mock_response.body.into_bytes().into(),
            closed: false,
        };
        HTTP_STREAMS.with(|streams| {
            let mut streams = streams.borrow_mut();
            if streams.len() >= MAX_HTTP_STREAMS {
                return Err(vm_error(format!(
                    "http_stream_open: maximum open streams ({MAX_HTTP_STREAMS}) reached"
                )));
            }
            streams.insert(id.clone(), handle);
            Ok(())
        })?;
        return Ok(VmValue::String(Rc::from(id)));
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(vm_error(format!(
            "http_stream_open: URL must start with http:// or https://, got '{url}'"
        )));
    }
    let config = parse_http_options(options);
    let client = if let Some(session_id) = session_from_options(options) {
        HTTP_SESSIONS
            .with(|sessions| sessions.borrow().get(&session_id).cloned())
            .map(|session| session.client)
            .ok_or_else(|| {
                vm_error(format!(
                    "http_stream_open: unknown HTTP session '{session_id}'"
                ))
            })?
    } else {
        pooled_http_client(&config)?
    };
    let mut request = client
        .request(parts.method, url)
        .headers(parts.headers)
        .timeout(Duration::from_millis(config.total_timeout_ms));
    if let Some(multipart) = &parts.multipart {
        request = request.multipart(multipart_form(multipart)?);
    } else if let Some(body) = parts.body {
        request = request.body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| vm_error(format!("http_stream_open: request failed: {error}")))?;
    verify_tls_pin(&response, &config.tls.pinned_sha256)?;
    let status = response.status().as_u16() as i64;
    let headers = response_headers(response.headers());
    let handle = HttpStreamHandle {
        kind: HttpStreamKind::Real(Rc::new(tokio::sync::Mutex::new(response))),
        status,
        headers,
        pending: VecDeque::new(),
        closed: false,
    };
    HTTP_STREAMS.with(|streams| {
        let mut streams = streams.borrow_mut();
        if streams.len() >= MAX_HTTP_STREAMS {
            return Err(vm_error(format!(
                "http_stream_open: maximum open streams ({MAX_HTTP_STREAMS}) reached"
            )));
        }
        streams.insert(id.clone(), handle);
        Ok(())
    })?;
    Ok(VmValue::String(Rc::from(id)))
}

async fn vm_http_stream_read(stream_id: &str, max_bytes: usize) -> Result<VmValue, VmError> {
    let (kind, mut pending, closed) = HTTP_STREAMS
        .with(|streams| {
            let mut streams = streams.borrow_mut();
            let handle = streams.get_mut(stream_id)?;
            let kind = match &handle.kind {
                HttpStreamKind::Real(response) => HttpStreamKind::Real(response.clone()),
                HttpStreamKind::Fake => HttpStreamKind::Fake,
            };
            let pending = std::mem::take(&mut handle.pending);
            Some((kind, pending, handle.closed))
        })
        .ok_or_else(|| vm_error(format!("http_stream_read: unknown stream '{stream_id}'")))?;
    if closed {
        return Ok(VmValue::Nil);
    }
    if pending.is_empty() {
        match kind {
            HttpStreamKind::Fake => {}
            HttpStreamKind::Real(response) => {
                let mut response = response.lock().await;
                if let Some(chunk) = response.chunk().await.map_err(|error| {
                    vm_error(format!(
                        "http_stream_read: failed to read response body: {error}"
                    ))
                })? {
                    pending.extend(chunk);
                }
            }
        }
    }
    if pending.is_empty() {
        HTTP_STREAMS.with(|streams| {
            if let Some(handle) = streams.borrow_mut().get_mut(stream_id) {
                handle.closed = true;
            }
        });
        return Ok(VmValue::Nil);
    }
    let take = pending.len().min(max_bytes.max(1));
    let chunk = pending.drain(..take).collect::<Vec<_>>();
    HTTP_STREAMS.with(|streams| {
        if let Some(handle) = streams.borrow_mut().get_mut(stream_id) {
            handle.pending = pending;
        }
    });
    Ok(VmValue::Bytes(Rc::new(chunk)))
}

fn vm_http_stream_info(stream_id: &str) -> Result<VmValue, VmError> {
    HTTP_STREAMS.with(|streams| {
        let streams = streams.borrow();
        let handle = streams
            .get(stream_id)
            .ok_or_else(|| vm_error(format!("http_stream_info: unknown stream '{stream_id}'")))?;
        let mut dict = BTreeMap::new();
        dict.insert("status".to_string(), VmValue::Int(handle.status));
        dict.insert(
            "headers".to_string(),
            VmValue::Dict(Rc::new(handle.headers.clone())),
        );
        dict.insert(
            "ok".to_string(),
            VmValue::Bool((200..300).contains(&(handle.status as u16))),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    })
}

async fn vm_sse_connect(
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let id = next_transport_handle("sse");
    let max_events =
        transport_limit_option(options, "max_events", DEFAULT_MAX_STREAM_EVENTS).max(1);
    let max_message_bytes =
        transport_limit_option(options, "max_message_bytes", DEFAULT_MAX_MESSAGE_BYTES).max(1);

    if let Some(events) = consume_sse_mock(url) {
        let handle = SseHandle {
            kind: SseHandleKind::Fake(Rc::new(tokio::sync::Mutex::new(FakeSseStream {
                events: events.into(),
                opened: false,
                closed: false,
            }))),
            url: url.to_string(),
            max_events,
            max_message_bytes,
            received: 0,
        };
        SSE_HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            if handles.len() >= MAX_SSE_STREAMS {
                return Err(vm_error(format!(
                    "sse_connect: maximum open streams ({MAX_SSE_STREAMS}) reached"
                )));
            }
            handles.insert(id.clone(), handle);
            Ok(())
        })?;
        record_transport_call(TransportMockCall {
            kind: "sse_connect".to_string(),
            handle: Some(id.clone()),
            url: url.to_string(),
            message_type: None,
            data: None,
        });
        return Ok(VmValue::String(Rc::from(id)));
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(vm_error(format!(
            "sse_connect: URL must start with http:// or https://, got '{url}'"
        )));
    }

    let config = parse_http_options(options);
    let client = if let Some(session_id) = session_from_options(options) {
        let session = HTTP_SESSIONS.with(|sessions| sessions.borrow().get(&session_id).cloned());
        session
            .map(|session| session.client)
            .ok_or_else(|| vm_error(format!("sse_connect: unknown HTTP session '{session_id}'")))?
    } else {
        pooled_http_client(&config)?
    };
    let parts = parse_http_request_parts(method, options)?;
    let mut request = client
        .request(parts.method, url)
        .headers(parts.headers)
        .timeout(Duration::from_millis(config.total_timeout_ms));
    if let Some(body) = parts.body {
        request = request.body(body);
    }
    let stream = EventSource::new(request)
        .map_err(|error| vm_error(format!("sse_connect: failed to create stream: {error}")))?;
    let handle = SseHandle {
        kind: SseHandleKind::Real(Rc::new(tokio::sync::Mutex::new(stream))),
        url: url.to_string(),
        max_events,
        max_message_bytes,
        received: 0,
    };
    SSE_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        if handles.len() >= MAX_SSE_STREAMS {
            return Err(vm_error(format!(
                "sse_connect: maximum open streams ({MAX_SSE_STREAMS}) reached"
            )));
        }
        handles.insert(id.clone(), handle);
        Ok(())
    })?;
    Ok(VmValue::String(Rc::from(id)))
}

async fn vm_sse_receive(stream_id: &str, timeout_ms: u64) -> Result<VmValue, VmError> {
    let stream = SSE_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles.get_mut(stream_id)?;
        if handle.received >= handle.max_events {
            return Some(Err(vm_error(format!(
                "sse_receive: stream '{stream_id}' exceeded max_events"
            ))));
        }
        handle.received += 1;
        let url = handle.url.clone();
        let max_message_bytes = handle.max_message_bytes;
        let kind = match &handle.kind {
            SseHandleKind::Real(stream) => SseHandleKind::Real(stream.clone()),
            SseHandleKind::Fake(stream) => SseHandleKind::Fake(stream.clone()),
        };
        Some(Ok((kind, url, max_message_bytes)))
    });
    let Some(stream) = stream else {
        return Err(vm_error(format!(
            "sse_receive: unknown stream '{stream_id}'"
        )));
    };
    let (kind, _url, max_message_bytes) = stream?;

    match kind {
        SseHandleKind::Fake(stream) => {
            let mut stream = stream.lock().await;
            if stream.closed {
                return Ok(VmValue::Nil);
            }
            if !stream.opened {
                stream.opened = true;
                let mut dict = BTreeMap::new();
                dict.insert("type".to_string(), VmValue::String(Rc::from("open")));
                return Ok(VmValue::Dict(Rc::new(dict)));
            }
            let Some(event) = stream.events.pop_front() else {
                stream.closed = true;
                return Ok(VmValue::Nil);
            };
            if event.data.len() > max_message_bytes {
                return Err(vm_error(format!(
                    "sse_receive: message exceeded max_message_bytes ({max_message_bytes})"
                )));
            }
            Ok(sse_event_value(&event))
        }
        SseHandleKind::Real(stream) => {
            let mut stream = stream.lock().await;
            let next = stream.next();
            let event = match tokio::time::timeout(Duration::from_millis(timeout_ms), next).await {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(error))) => {
                    return Err(vm_error(format!("sse_receive: stream error: {error}")));
                }
                Ok(None) => return Ok(VmValue::Nil),
                Err(_) => return Ok(timeout_event()),
            };
            if let SseEvent::Message(message) = &event {
                if message.data.len() > max_message_bytes {
                    stream.close();
                    return Err(vm_error(format!(
                        "sse_receive: message exceeded max_message_bytes ({max_message_bytes})"
                    )));
                }
            }
            Ok(real_sse_event_value(event))
        }
    }
}

async fn vm_websocket_connect(
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let id = next_transport_handle("websocket");
    let max_messages =
        transport_limit_option(options, "max_messages", DEFAULT_MAX_STREAM_EVENTS).max(1);
    let max_message_bytes =
        transport_limit_option(options, "max_message_bytes", DEFAULT_MAX_MESSAGE_BYTES).max(1);

    if let Some((messages, echo)) = consume_websocket_mock(url) {
        let handle = WebSocketHandle {
            kind: WebSocketHandleKind::Fake(Rc::new(tokio::sync::Mutex::new(FakeWebSocket {
                messages: messages.into(),
                echo,
                closed: false,
            }))),
            url: url.to_string(),
            max_messages,
            max_message_bytes,
            received: 0,
        };
        WEBSOCKET_HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            if handles.len() >= MAX_WEBSOCKETS {
                return Err(vm_error(format!(
                    "websocket_connect: maximum open sockets ({MAX_WEBSOCKETS}) reached"
                )));
            }
            handles.insert(id.clone(), handle);
            Ok(())
        })?;
        record_transport_call(TransportMockCall {
            kind: "websocket_connect".to_string(),
            handle: Some(id.clone()),
            url: url.to_string(),
            message_type: None,
            data: None,
        });
        return Ok(VmValue::String(Rc::from(id)));
    }

    if !url.starts_with("ws://") && !url.starts_with("wss://") {
        return Err(vm_error(format!(
            "websocket_connect: URL must start with ws:// or wss://, got '{url}'"
        )));
    }
    let timeout_ms = vm_get_int_option_prefer(
        options,
        "timeout_ms",
        "timeout",
        DEFAULT_TIMEOUT_MS as i64,
    )
    .max(0) as u64;
    let connect = tokio_tungstenite::connect_async(url);
    let (socket, _) = tokio::time::timeout(Duration::from_millis(timeout_ms), connect)
        .await
        .map_err(|_| vm_error(format!("websocket_connect: timed out after {timeout_ms}ms")))?
        .map_err(|error| vm_error(format!("websocket_connect: failed: {error}")))?;
    let handle = WebSocketHandle {
        kind: WebSocketHandleKind::Real(Rc::new(tokio::sync::Mutex::new(socket))),
        url: url.to_string(),
        max_messages,
        max_message_bytes,
        received: 0,
    };
    WEBSOCKET_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        if handles.len() >= MAX_WEBSOCKETS {
            return Err(vm_error(format!(
                "websocket_connect: maximum open sockets ({MAX_WEBSOCKETS}) reached"
            )));
        }
        handles.insert(id.clone(), handle);
        Ok(())
    })?;
    Ok(VmValue::String(Rc::from(id)))
}

fn websocket_message_from_vm(
    value: VmValue,
    options: &BTreeMap<String, VmValue>,
) -> Result<MockWsMessage, VmError> {
    let message_type = options
        .get("type")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| match value {
            VmValue::Bytes(_) => "binary".to_string(),
            _ => "text".to_string(),
        });
    let data = match value {
        VmValue::Bytes(bytes) => bytes.as_ref().clone(),
        other
            if options
                .get("base64")
                .and_then(|value| match value {
                    VmValue::Bool(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(false) =>
        {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(other.display())
                .map_err(|error| vm_error(format!("websocket_send: invalid base64: {error}")))?
        }
        other => other.display().into_bytes(),
    };
    Ok(MockWsMessage { message_type, data })
}

fn real_ws_message(message: &MockWsMessage) -> Result<WsMessage, VmError> {
    match message.message_type.as_str() {
        "text" => Ok(WsMessage::Text(
            String::from_utf8(message.data.clone())
                .map_err(|error| vm_error(format!("websocket_send: text is not UTF-8: {error}")))?
                .into(),
        )),
        "binary" => Ok(WsMessage::Binary(message.data.clone().into())),
        "ping" => Ok(WsMessage::Ping(message.data.clone().into())),
        "pong" => Ok(WsMessage::Pong(message.data.clone().into())),
        "close" => Ok(WsMessage::Close(None)),
        other => Err(vm_error(format!(
            "websocket_send: unsupported message type '{other}'"
        ))),
    }
}

async fn vm_websocket_send(
    socket_id: &str,
    value: VmValue,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let message = websocket_message_from_vm(value, options)?;
    let socket = WEBSOCKET_HANDLES.with(|handles| {
        let handles = handles.borrow();
        let handle = handles.get(socket_id)?;
        let url = handle.url.clone();
        let max_message_bytes = handle.max_message_bytes;
        let kind = match &handle.kind {
            WebSocketHandleKind::Real(socket) => WebSocketHandleKind::Real(socket.clone()),
            WebSocketHandleKind::Fake(socket) => WebSocketHandleKind::Fake(socket.clone()),
        };
        Some((kind, url, max_message_bytes))
    });
    let Some((kind, url, max_message_bytes)) = socket else {
        return Err(vm_error(format!(
            "websocket_send: unknown socket '{socket_id}'"
        )));
    };
    if message.data.len() > max_message_bytes {
        return Err(vm_error(format!(
            "websocket_send: message exceeded max_message_bytes ({max_message_bytes})"
        )));
    }
    match kind {
        WebSocketHandleKind::Fake(socket) => {
            let mut socket = socket.lock().await;
            if socket.closed {
                return Ok(VmValue::Bool(false));
            }
            if message.message_type == "close" {
                socket.closed = true;
            } else if socket.echo {
                socket.messages.push_back(message.clone());
            }
            record_transport_call(TransportMockCall {
                kind: "websocket_send".to_string(),
                handle: Some(socket_id.to_string()),
                url,
                message_type: Some(message.message_type.clone()),
                data: Some(ws_message_data(&message)),
            });
            Ok(VmValue::Bool(true))
        }
        WebSocketHandleKind::Real(socket) => {
            let mut socket = socket.lock().await;
            socket
                .send(real_ws_message(&message)?)
                .await
                .map_err(|error| vm_error(format!("websocket_send: failed: {error}")))?;
            Ok(VmValue::Bool(true))
        }
    }
}

async fn vm_websocket_receive(socket_id: &str, timeout_ms: u64) -> Result<VmValue, VmError> {
    let socket = WEBSOCKET_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles.get_mut(socket_id)?;
        if handle.received >= handle.max_messages {
            return Some(Err(vm_error(format!(
                "websocket_receive: socket '{socket_id}' exceeded max_messages"
            ))));
        }
        handle.received += 1;
        let max_message_bytes = handle.max_message_bytes;
        let kind = match &handle.kind {
            WebSocketHandleKind::Real(socket) => WebSocketHandleKind::Real(socket.clone()),
            WebSocketHandleKind::Fake(socket) => WebSocketHandleKind::Fake(socket.clone()),
        };
        Some(Ok((kind, max_message_bytes)))
    });
    let Some(socket) = socket else {
        return Err(vm_error(format!(
            "websocket_receive: unknown socket '{socket_id}'"
        )));
    };
    let (kind, max_message_bytes) = socket?;
    match kind {
        WebSocketHandleKind::Fake(socket) => {
            let mut socket = socket.lock().await;
            if socket.closed {
                return Ok(VmValue::Nil);
            }
            let Some(message) = socket.messages.pop_front() else {
                return Ok(timeout_event());
            };
            if message.data.len() > max_message_bytes {
                socket.closed = true;
                return Err(vm_error(format!(
                    "websocket_receive: message exceeded max_message_bytes ({max_message_bytes})"
                )));
            }
            if message.message_type == "close" {
                socket.closed = true;
            }
            Ok(ws_event_value(message))
        }
        WebSocketHandleKind::Real(socket) => {
            let mut socket = socket.lock().await;
            let next = socket.next();
            let message = match tokio::time::timeout(Duration::from_millis(timeout_ms), next).await
            {
                Ok(Some(Ok(message))) => message,
                Ok(Some(Err(error))) => {
                    return Err(vm_error(format!("websocket_receive: failed: {error}")));
                }
                Ok(None) => return Ok(VmValue::Nil),
                Err(_) => return Ok(timeout_event()),
            };
            match &message {
                WsMessage::Text(text) if text.len() > max_message_bytes => {
                    return Err(vm_error(format!(
                        "websocket_receive: message exceeded max_message_bytes ({max_message_bytes})"
                    )));
                }
                WsMessage::Binary(bytes) | WsMessage::Ping(bytes) | WsMessage::Pong(bytes)
                    if bytes.len() > max_message_bytes =>
                {
                    return Err(vm_error(format!(
                        "websocket_receive: message exceeded max_message_bytes ({max_message_bytes})"
                    )));
                }
                _ => {}
            }
            Ok(real_ws_event_value(message))
        }
    }
}

async fn vm_websocket_close(socket_id: &str) -> Result<VmValue, VmError> {
    let removed = WEBSOCKET_HANDLES.with(|handles| handles.borrow_mut().remove(socket_id));
    let Some(handle) = removed else {
        return Ok(VmValue::Bool(false));
    };
    match handle.kind {
        WebSocketHandleKind::Fake(socket) => {
            socket.lock().await.closed = true;
            record_transport_call(TransportMockCall {
                kind: "websocket_close".to_string(),
                handle: Some(socket_id.to_string()),
                url: handle.url,
                message_type: None,
                data: None,
            });
            Ok(VmValue::Bool(true))
        }
        WebSocketHandleKind::Real(socket) => {
            let mut socket = socket.lock().await;
            socket
                .close(None)
                .await
                .map_err(|error| vm_error(format!("websocket_close: failed: {error}")))?;
            Ok(VmValue::Bool(true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_retry_delay, http_mock_calls_snapshot, parse_retry_after_value, push_http_mock,
        reset_http_state, vm_execute_http_request, vm_http_download, vm_http_stream_info,
        vm_http_stream_open, vm_http_stream_read, HttpMockResponse,
    };
    use crate::connectors::test_util::{
        accept_http_connection, read_http_request, write_http_response,
    };
    use crate::value::VmValue;
    use base64::Engine;
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use rustls::{ServerConfig, ServerConnection, StreamOwned};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;
    use std::sync::{Arc, Once};
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;
    use x509_parser::prelude::{FromDer, X509Certificate};

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

    #[tokio::test]
    async fn typed_mock_api_drives_http_request_retries() {
        reset_http_state();
        push_http_mock(
            "GET",
            "https://api.example.com/retry",
            vec![
                HttpMockResponse::new(503, "busy").with_header("retry-after", "0"),
                HttpMockResponse::new(200, "ok"),
            ],
        );
        let result = vm_execute_http_request(
            "GET",
            "https://api.example.com/retry",
            &BTreeMap::from([
                ("retries".to_string(), VmValue::Int(1)),
                ("backoff".to_string(), VmValue::Int(0)),
            ]),
        )
        .await
        .expect("mocked request should succeed after retry");

        let dict = result.as_dict().expect("response dict");
        assert_eq!(dict["status"].as_int(), Some(200));
        let calls = http_mock_calls_snapshot();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].url, "https://api.example.com/retry");
        reset_http_state();
    }

    #[tokio::test]
    async fn multipart_requests_are_mock_visible() {
        reset_http_state();
        push_http_mock(
            "POST",
            "https://api.example.com/upload",
            vec![HttpMockResponse::new(201, "uploaded")],
        );
        let options = BTreeMap::from([(
            "multipart".to_string(),
            VmValue::List(Rc::new(vec![
                VmValue::Dict(Rc::new(BTreeMap::from([
                    ("name".to_string(), VmValue::String(Rc::from("meta"))),
                    ("value".to_string(), VmValue::String(Rc::from("hello"))),
                ]))),
                VmValue::Dict(Rc::new(BTreeMap::from([
                    ("name".to_string(), VmValue::String(Rc::from("blob"))),
                    (
                        "filename".to_string(),
                        VmValue::String(Rc::from("blob.bin")),
                    ),
                    (
                        "content_type".to_string(),
                        VmValue::String(Rc::from("application/octet-stream")),
                    ),
                    (
                        "value".to_string(),
                        VmValue::Bytes(Rc::new(vec![0, 1, 2, 3])),
                    ),
                ]))),
            ])),
        )]);

        let response = vm_execute_http_request("POST", "https://api.example.com/upload", &options)
            .await
            .expect("multipart mock request should succeed");
        let response = response.as_dict().expect("response dict");
        assert_eq!(response["status"].as_int(), Some(201));

        let calls = http_mock_calls_snapshot();
        assert_eq!(calls.len(), 1);
        assert!(calls[0]
            .headers
            .get("content-type")
            .expect("content-type recorded")
            .contains("multipart/form-data"));
        let body = calls[0].body.as_deref().expect("multipart body recorded");
        assert!(body.contains("name=\"meta\""));
        assert!(body.contains("hello"));
        assert!(body.contains("filename=\"blob.bin\""));
        reset_http_state();
    }

    #[tokio::test]
    async fn http_stream_mock_reads_in_chunks() {
        reset_http_state();
        push_http_mock(
            "GET",
            "https://api.example.com/stream",
            vec![HttpMockResponse::new(200, "stream-body")],
        );

        let handle = vm_http_stream_open("https://api.example.com/stream", &BTreeMap::new())
            .await
            .expect("stream open");
        let stream_id = handle.display();
        let info = vm_http_stream_info(&stream_id).expect("stream info");
        let info = info.as_dict().expect("info dict");
        assert_eq!(info["status"].as_int(), Some(200));

        let first = vm_http_stream_read(&stream_id, 6)
            .await
            .expect("first chunk");
        let second = vm_http_stream_read(&stream_id, 64)
            .await
            .expect("second chunk");
        let end = vm_http_stream_read(&stream_id, 64)
            .await
            .expect("end marker");
        assert_eq!(first.as_bytes().expect("bytes"), b"stream");
        assert_eq!(second.as_bytes().expect("bytes"), b"-body");
        assert!(matches!(end, VmValue::Nil));
        reset_http_state();
    }

    #[tokio::test]
    async fn http_download_mock_writes_file() {
        reset_http_state();
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("download.bin");
        push_http_mock(
            "GET",
            "https://api.example.com/download",
            vec![HttpMockResponse::new(200, "downloaded")],
        );

        let response = vm_http_download(
            "https://api.example.com/download",
            &path.display().to_string(),
            &BTreeMap::new(),
        )
        .await
        .expect("download response");
        let response = response.as_dict().expect("response dict");
        assert_eq!(response["bytes_written"].as_int(), Some(10));
        assert_eq!(
            std::fs::read_to_string(path).expect("downloaded file"),
            "downloaded"
        );
        reset_http_state();
    }

    #[tokio::test]
    async fn http_proxy_routes_requests_through_configured_proxy() {
        reset_http_state();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
        let proxy_addr = listener.local_addr().expect("proxy addr");
        let proxy_thread = std::thread::spawn(move || {
            let mut stream = accept_http_connection(&listener, "proxy listener");
            let request = read_http_request(&mut stream);
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "http://example.invalid/proxy-check");
            assert_eq!(
                request
                    .headers
                    .get("proxy-authorization")
                    .map(String::as_str),
                Some("Basic dXNlcjpwYXNz")
            );
            write_http_response(
                &mut stream,
                200,
                &[("content-type", "text/plain".to_string())],
                "proxied",
            );
        });

        let options = BTreeMap::from([
            (
                "proxy".to_string(),
                VmValue::String(Rc::from(format!("http://{proxy_addr}"))),
            ),
            (
                "proxy_auth".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::from([
                    ("user".to_string(), VmValue::String(Rc::from("user"))),
                    ("pass".to_string(), VmValue::String(Rc::from("pass"))),
                ]))),
            ),
            ("timeout_ms".to_string(), VmValue::Int(1_000)),
        ]);

        let response =
            vm_execute_http_request("GET", "http://example.invalid/proxy-check", &options)
                .await
                .expect("proxied response");
        let response = response.as_dict().expect("response dict");
        assert_eq!(response["status"].as_int(), Some(200));
        assert_eq!(response["body"].display(), "proxied");
        proxy_thread.join().expect("proxy thread");
        reset_http_state();
    }

    #[tokio::test]
    async fn custom_tls_ca_bundle_and_pin_allow_request() {
        reset_http_state();
        install_rustls_provider();
        let temp = TempDir::new().expect("tempdir");
        let cert =
            generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .expect("generate cert");
        let cert_pem = cert.cert.pem();
        let cert_path = temp.path().join("cert.pem");
        std::fs::write(&cert_path, cert_pem.as_bytes()).expect("write cert");
        let pin = spki_pin_base64(cert.cert.der().as_ref());

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
        let port = listener.local_addr().expect("tls addr").port();
        let server_config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
                )
                .expect("build tls config"),
        );
        let thread = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().expect("accept tls client");
            let conn = ServerConnection::new(server_config).expect("server connection");
            let mut stream = StreamOwned::new(conn, tcp);
            let request = read_http_request_generic(&mut stream);
            assert!(request.starts_with("GET /secure HTTP/1.1\r\n"));
            write_http_response_generic(
                &mut stream,
                200,
                &[("content-type", "text/plain".to_string())],
                "secure",
            );
        });

        let options = BTreeMap::from([(
            "tls".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "ca_bundle_path".to_string(),
                    VmValue::String(Rc::from(cert_path.display().to_string())),
                ),
                (
                    "pinned_sha256".to_string(),
                    VmValue::List(Rc::new(vec![VmValue::String(Rc::from(pin))])),
                ),
            ]))),
        )]);
        let response =
            vm_execute_http_request("GET", &format!("https://localhost:{port}/secure"), &options)
                .await
                .expect("tls request should succeed");
        let response = response.as_dict().expect("response dict");
        assert_eq!(response["body"].display(), "secure");
        thread.join().expect("tls thread");
        reset_http_state();
    }

    #[tokio::test]
    async fn custom_tls_pin_mismatch_is_rejected() {
        reset_http_state();
        install_rustls_provider();
        let temp = TempDir::new().expect("tempdir");
        let cert =
            generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .expect("generate cert");
        let cert_pem = cert.cert.pem();
        let cert_path = temp.path().join("cert.pem");
        std::fs::write(&cert_path, cert_pem.as_bytes()).expect("write cert");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
        let port = listener.local_addr().expect("tls addr").port();
        let server_config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
                )
                .expect("build tls config"),
        );
        let thread = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().expect("accept tls client");
            let conn = ServerConnection::new(server_config).expect("server connection");
            let mut stream = StreamOwned::new(conn, tcp);
            let _ = read_http_request_generic(&mut stream);
            write_http_response_generic(
                &mut stream,
                200,
                &[("content-type", "text/plain".to_string())],
                "secure",
            );
        });

        let options = BTreeMap::from([(
            "tls".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "ca_bundle_path".to_string(),
                    VmValue::String(Rc::from(cert_path.display().to_string())),
                ),
                (
                    "pinned_sha256".to_string(),
                    VmValue::List(Rc::new(vec![VmValue::String(Rc::from("deadbeef"))])),
                ),
            ]))),
        )]);
        let error =
            vm_execute_http_request("GET", &format!("https://localhost:{port}/secure"), &options)
                .await
                .expect_err("pin mismatch should fail");
        let message = error.to_string();
        assert!(message.contains("TLS SPKI pin mismatch"), "{message}");
        thread.join().expect("tls thread");
        reset_http_state();
    }

    fn install_rustls_provider() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    fn spki_pin_base64(cert_der: &[u8]) -> String {
        let (_, cert) = X509Certificate::from_der(cert_der).expect("parse cert");
        base64::engine::general_purpose::STANDARD
            .encode(Sha256::digest(cert.tbs_certificate.subject_pki.raw))
    }

    fn read_http_request_generic<T: Read>(stream: &mut T) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).expect("read request");
            assert!(read > 0, "request closed before headers");
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8_lossy(&buffer).into_owned();
            }
        }
    }

    fn write_http_response_generic<T: Write>(
        stream: &mut T,
        status: u16,
        headers: &[(&str, String)],
        body: &str,
    ) {
        let mut response = format!(
            "HTTP/1.1 {status} OK\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
    }
}
