use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::TcpStream;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, RwLock,
};
use std::time::Duration;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::client::{
    clear_http_streams, parse_http_options, parse_http_request_parts, pooled_http_client,
    session_from_options, HTTP_SESSIONS,
};
use super::mock::url_matches;
use super::{
    get_options_arg, handle_from_value, next_transport_handle, vm_error, DEFAULT_MAX_MESSAGE_BYTES,
    DEFAULT_MAX_STREAM_EVENTS, DEFAULT_TRANSPORT_RECEIVE_TIMEOUT_MS, MAX_SSE_SERVER_STREAMS,
    MAX_SSE_STREAMS,
};
use futures::StreamExt;
use reqwest_eventsource::{Event as SseEvent, EventSource};

mod websocket;

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

pub(super) struct SseServerHandle {
    status: i64,
    headers: BTreeMap<String, VmValue>,
    frames: VecDeque<String>,
    max_event_bytes: usize,
    max_buffered_events: usize,
    sent_events: usize,
    flushed_events: usize,
    closed: bool,
    pub(super) disconnected: bool,
    pub(super) cancelled: bool,
    cancel_reason: Option<String>,
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
    close_code: Option<u16>,
    close_reason: Option<String>,
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
    Server(Rc<tokio::sync::Mutex<ServerWebSocket>>),
}

struct FakeWebSocket {
    messages: VecDeque<MockWsMessage>,
    echo: bool,
    closed: bool,
}

struct WebSocketServer {
    addr: String,
    routes: Arc<RwLock<HashMap<String, WebSocketRoute>>>,
    events: Rc<tokio::sync::Mutex<mpsc::Receiver<WebSocketServerEvent>>>,
    running: Arc<AtomicBool>,
}

#[derive(Clone)]
struct WebSocketRoute {
    path: String,
    bearer_token: Option<String>,
    max_messages: usize,
    max_message_bytes: usize,
    send_buffer_messages: usize,
    idle_timeout_ms: u64,
}

struct WebSocketServerEvent {
    handle: ServerWebSocket,
    path: String,
    peer: String,
    headers: BTreeMap<String, String>,
    max_messages: usize,
    max_message_bytes: usize,
}

struct ServerWebSocket {
    incoming: VecDeque<MockWsMessage>,
    incoming_rx: mpsc::Receiver<MockWsMessage>,
    outgoing_tx: mpsc::SyncSender<ServerWebSocketCommand>,
    closed: bool,
}

enum ServerWebSocketCommand {
    Send(MockWsMessage),
    Close(Option<u16>, Option<String>),
}

#[derive(Clone)]
struct TransportMockCall {
    kind: String,
    handle: Option<String>,
    url: String,
    message_type: Option<String>,
    data: Option<String>,
}

thread_local! {
    static SSE_MOCKS: RefCell<Vec<SseMock>> = const { RefCell::new(Vec::new()) };
    static SSE_HANDLES: RefCell<HashMap<String, SseHandle>> = RefCell::new(HashMap::new());
    static SSE_SERVER_HANDLES: RefCell<HashMap<String, SseServerHandle>> = RefCell::new(HashMap::new());
    static WEBSOCKET_MOCKS: RefCell<Vec<WebSocketMock>> = const { RefCell::new(Vec::new()) };
    static WEBSOCKET_HANDLES: RefCell<HashMap<String, WebSocketHandle>> = RefCell::new(HashMap::new());
    static WEBSOCKET_SERVERS: RefCell<HashMap<String, WebSocketServer>> = RefCell::new(HashMap::new());
    static TRANSPORT_MOCK_CALLS: RefCell<Vec<TransportMockCall>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn reset_streaming_state() {
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
    SSE_SERVER_HANDLES.with(|handles| handles.borrow_mut().clear());
    WEBSOCKET_MOCKS.with(|mocks| mocks.borrow_mut().clear());
    WEBSOCKET_HANDLES.with(|handles| handles.borrow_mut().clear());
    close_websocket_servers();
    TRANSPORT_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
}

fn close_websocket_servers() {
    WEBSOCKET_SERVERS.with(|servers| {
        let mut servers = servers.borrow_mut();
        for server in servers.values() {
            server.running.store(false, Ordering::SeqCst);
            let _ = TcpStream::connect(&server.addr);
        }
        servers.clear();
    });
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

fn sse_server_closed_event() -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("type".to_string(), VmValue::String(Rc::from("close")));
    dict.insert("server_closed".to_string(), VmValue::Bool(true));
    VmValue::Dict(Rc::new(dict))
}

fn record_transport_call(call: TransportMockCall) {
    TRANSPORT_MOCK_CALLS.with(|calls| calls.borrow_mut().push(call));
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

fn sse_server_response_value(id: &str, handle: &SseServerHandle) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("id".to_string(), VmValue::String(Rc::from(id)));
    dict.insert(
        "type".to_string(),
        VmValue::String(Rc::from("sse_response")),
    );
    dict.insert("status".to_string(), VmValue::Int(handle.status));
    dict.insert(
        "headers".to_string(),
        VmValue::Dict(Rc::new(handle.headers.clone())),
    );
    dict.insert("body".to_string(), VmValue::Nil);
    dict.insert("streaming".to_string(), VmValue::Bool(true));
    dict.insert(
        "max_event_bytes".to_string(),
        VmValue::Int(handle.max_event_bytes as i64),
    );
    dict.insert(
        "max_buffered_events".to_string(),
        VmValue::Int(handle.max_buffered_events as i64),
    );
    VmValue::Dict(Rc::new(dict))
}

fn default_sse_response_headers() -> BTreeMap<String, VmValue> {
    BTreeMap::from([
        (
            "content-type".to_string(),
            VmValue::String(Rc::from("text/event-stream; charset=utf-8")),
        ),
        (
            "cache-control".to_string(),
            VmValue::String(Rc::from("no-cache")),
        ),
        (
            "connection".to_string(),
            VmValue::String(Rc::from("keep-alive")),
        ),
        (
            "x-accel-buffering".to_string(),
            VmValue::String(Rc::from("no")),
        ),
    ])
}

fn sse_response_headers(options: &BTreeMap<String, VmValue>) -> BTreeMap<String, VmValue> {
    let mut headers = default_sse_response_headers();
    if let Some(VmValue::Dict(custom)) = options.get("headers") {
        for (name, value) in custom.iter() {
            headers.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
            headers.insert(name.clone(), VmValue::String(Rc::from(value.display())));
        }
    }
    if !headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("content-type"))
    {
        headers.insert(
            "content-type".to_string(),
            VmValue::String(Rc::from("text/event-stream; charset=utf-8")),
        );
    }
    headers
}

fn validate_sse_field(field: &str, value: &str) -> Result<(), VmError> {
    if value.contains('\n') || value.contains('\r') {
        return Err(vm_error(format!(
            "sse_event: {field} must not contain newlines"
        )));
    }
    Ok(())
}

fn push_sse_multiline_field(frame: &mut String, field: &str, value: &str) {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        frame.push_str(field);
        frame.push_str(": \n");
        return;
    }
    for line in normalized.split('\n') {
        frame.push_str(field);
        frame.push_str(": ");
        frame.push_str(line);
        frame.push('\n');
    }
}

pub(super) fn vm_sse_event_frame(
    event: &VmValue,
    options: &BTreeMap<String, VmValue>,
) -> Result<String, VmError> {
    let mut frame = String::new();
    let mut has_event_payload = false;

    match event {
        VmValue::Dict(dict) => {
            if let Some(comment) = dict.get("comment").or_else(|| options.get("comment")) {
                push_sse_comment(&mut frame, &comment.display());
            }
            if let Some(id) = dict.get("id").or_else(|| options.get("id")) {
                let id = id.display();
                validate_sse_field("id", &id)?;
                frame.push_str("id: ");
                frame.push_str(&id);
                frame.push('\n');
            }
            if let Some(event_type) = dict
                .get("event")
                .or_else(|| dict.get("name"))
                .or_else(|| options.get("event"))
            {
                let event_type = event_type.display();
                validate_sse_field("event", &event_type)?;
                frame.push_str("event: ");
                frame.push_str(&event_type);
                frame.push('\n');
                has_event_payload = true;
            }
            if let Some(retry) = dict
                .get("retry")
                .or_else(|| dict.get("retry_ms"))
                .or_else(|| options.get("retry"))
                .or_else(|| options.get("retry_ms"))
            {
                let retry_ms = retry.as_int().ok_or_else(|| {
                    vm_error("sse_event: retry/retry_ms must be a non-negative integer")
                })?;
                if retry_ms < 0 {
                    return Err(vm_error(
                        "sse_event: retry/retry_ms must be a non-negative integer",
                    ));
                }
                frame.push_str("retry: ");
                frame.push_str(&retry_ms.to_string());
                frame.push('\n');
                has_event_payload = true;
            }
            if let Some(data) = dict.get("data").or_else(|| options.get("data")) {
                push_sse_multiline_field(&mut frame, "data", &data.display());
                has_event_payload = true;
            } else if !frame.is_empty() && !dict.contains_key("comment") {
                push_sse_multiline_field(&mut frame, "data", "");
                has_event_payload = true;
            }
        }
        other => {
            if let Some(comment) = options.get("comment") {
                push_sse_comment(&mut frame, &comment.display());
            }
            if let Some(id) = options.get("id") {
                let id = id.display();
                validate_sse_field("id", &id)?;
                frame.push_str("id: ");
                frame.push_str(&id);
                frame.push('\n');
            }
            if let Some(event_type) = options.get("event") {
                let event_type = event_type.display();
                validate_sse_field("event", &event_type)?;
                frame.push_str("event: ");
                frame.push_str(&event_type);
                frame.push('\n');
            }
            if let Some(retry) = options.get("retry").or_else(|| options.get("retry_ms")) {
                let retry_ms = retry.as_int().ok_or_else(|| {
                    vm_error("sse_event: retry/retry_ms must be a non-negative integer")
                })?;
                if retry_ms < 0 {
                    return Err(vm_error(
                        "sse_event: retry/retry_ms must be a non-negative integer",
                    ));
                }
                frame.push_str("retry: ");
                frame.push_str(&retry_ms.to_string());
                frame.push('\n');
            }
            push_sse_multiline_field(&mut frame, "data", &other.display());
            has_event_payload = true;
        }
    }

    if frame.is_empty() || !has_event_payload && !frame.starts_with(':') {
        push_sse_comment(&mut frame, "");
    }
    frame.push('\n');
    Ok(frame)
}

fn push_sse_comment(frame: &mut String, comment: &str) {
    let normalized = comment.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        frame.push_str(":\n");
        return;
    }
    for line in normalized.split('\n') {
        frame.push_str(": ");
        frame.push_str(line);
        frame.push('\n');
    }
}

pub(super) fn vm_sse_server_response(
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let id = next_transport_handle("sse-server");
    let status = options
        .get("status")
        .and_then(|value| value.as_int())
        .unwrap_or(200)
        .clamp(100, 599);
    let handle = SseServerHandle {
        status,
        headers: sse_response_headers(options),
        frames: VecDeque::new(),
        max_event_bytes: transport_limit_option(
            options,
            "max_event_bytes",
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .max(1),
        max_buffered_events: transport_limit_option(
            options,
            "max_buffered_events",
            DEFAULT_MAX_STREAM_EVENTS,
        )
        .max(1),
        sent_events: 0,
        flushed_events: 0,
        closed: false,
        disconnected: false,
        cancelled: false,
        cancel_reason: None,
    };
    let value = sse_server_response_value(&id, &handle);
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        if handles.len() >= MAX_SSE_SERVER_STREAMS {
            return Err(vm_error(format!(
                "sse_server_response: maximum open streams ({MAX_SSE_SERVER_STREAMS}) reached"
            )));
        }
        handles.insert(id, handle);
        Ok(())
    })?;
    Ok(value)
}

fn sse_server_status_value(id: &str, handle: &SseServerHandle) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("id".to_string(), VmValue::String(Rc::from(id)));
    dict.insert("status".to_string(), VmValue::Int(handle.status));
    dict.insert(
        "headers".to_string(),
        VmValue::Dict(Rc::new(handle.headers.clone())),
    );
    dict.insert(
        "buffered_events".to_string(),
        VmValue::Int(handle.frames.len() as i64),
    );
    dict.insert(
        "sent_events".to_string(),
        VmValue::Int(handle.sent_events as i64),
    );
    dict.insert(
        "flushed_events".to_string(),
        VmValue::Int(handle.flushed_events as i64),
    );
    dict.insert("closed".to_string(), VmValue::Bool(handle.closed));
    dict.insert(
        "disconnected".to_string(),
        VmValue::Bool(handle.disconnected),
    );
    dict.insert("cancelled".to_string(), VmValue::Bool(handle.cancelled));
    dict.insert(
        "cancel_reason".to_string(),
        handle
            .cancel_reason
            .as_deref()
            .map(|reason| VmValue::String(Rc::from(reason)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "max_event_bytes".to_string(),
        VmValue::Int(handle.max_event_bytes as i64),
    );
    dict.insert(
        "max_buffered_events".to_string(),
        VmValue::Int(handle.max_buffered_events as i64),
    );
    VmValue::Dict(Rc::new(dict))
}

pub(super) fn vm_sse_server_status(stream_id: &str) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        handles
            .borrow()
            .get(stream_id)
            .map(|handle| sse_server_status_value(stream_id, handle))
            .ok_or_else(|| vm_error(format!("sse_server_status: unknown stream '{stream_id}'")))
    })
}

pub(super) fn vm_sse_server_send(
    stream_id: &str,
    event: &VmValue,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let frame = vm_sse_event_frame(event, options)?;
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles
            .get_mut(stream_id)
            .ok_or_else(|| vm_error(format!("sse_server_send: unknown stream '{stream_id}'")))?;
        if handle.closed || handle.cancelled || handle.disconnected {
            return Ok(VmValue::Bool(false));
        }
        if frame.len() > handle.max_event_bytes {
            return Err(vm_error(format!(
                "sse_server_send: event exceeded max_event_bytes ({})",
                handle.max_event_bytes
            )));
        }
        if handle.frames.len() >= handle.max_buffered_events {
            return Err(vm_error(format!(
                "sse_server_send: buffered events exceeded max_buffered_events ({})",
                handle.max_buffered_events
            )));
        }
        handle.frames.push_back(frame);
        handle.sent_events += 1;
        Ok(VmValue::Bool(true))
    })
}

pub(super) fn vm_sse_server_heartbeat(
    stream_id: &str,
    comment: Option<&VmValue>,
) -> Result<VmValue, VmError> {
    let mut frame = String::new();
    push_sse_comment(
        &mut frame,
        &comment
            .map(|value| value.display())
            .unwrap_or_else(|| "heartbeat".to_string()),
    );
    frame.push('\n');
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles.get_mut(stream_id).ok_or_else(|| {
            vm_error(format!(
                "sse_server_heartbeat: unknown stream '{stream_id}'"
            ))
        })?;
        if handle.closed || handle.cancelled || handle.disconnected {
            return Ok(VmValue::Bool(false));
        }
        if frame.len() > handle.max_event_bytes {
            return Err(vm_error(format!(
                "sse_server_heartbeat: event exceeded max_event_bytes ({})",
                handle.max_event_bytes
            )));
        }
        if handle.frames.len() >= handle.max_buffered_events {
            return Err(vm_error(format!(
                "sse_server_heartbeat: buffered events exceeded max_buffered_events ({})",
                handle.max_buffered_events
            )));
        }
        handle.frames.push_back(frame);
        handle.sent_events += 1;
        Ok(VmValue::Bool(true))
    })
}

pub(super) fn vm_sse_server_flush(stream_id: &str) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles
            .get_mut(stream_id)
            .ok_or_else(|| vm_error(format!("sse_server_flush: unknown stream '{stream_id}'")))?;
        if handle.disconnected || handle.cancelled {
            return Ok(VmValue::Bool(false));
        }
        handle.flushed_events = handle.sent_events;
        Ok(VmValue::Bool(!handle.closed))
    })
}

pub(super) fn vm_sse_server_close(stream_id: &str) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles
            .get_mut(stream_id)
            .ok_or_else(|| vm_error(format!("sse_server_close: unknown stream '{stream_id}'")))?;
        if handle.closed {
            return Ok(VmValue::Bool(false));
        }
        handle.closed = true;
        Ok(VmValue::Bool(true))
    })
}

pub(super) fn vm_sse_server_cancel(
    stream_id: &str,
    reason: Option<&VmValue>,
) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles
            .get_mut(stream_id)
            .ok_or_else(|| vm_error(format!("sse_server_cancel: unknown stream '{stream_id}'")))?;
        if handle.cancelled {
            return Ok(VmValue::Bool(false));
        }
        handle.cancelled = true;
        handle.closed = true;
        handle.cancel_reason = reason
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
        Ok(VmValue::Bool(true))
    })
}

pub(super) fn vm_sse_server_observed_bool(
    stream_id: &str,
    builtin: &str,
    predicate: impl Fn(&SseServerHandle) -> bool,
) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        handles
            .borrow()
            .get(stream_id)
            .map(|handle| VmValue::Bool(predicate(handle)))
            .ok_or_else(|| vm_error(format!("{builtin}: unknown stream '{stream_id}'")))
    })
}

pub(super) fn vm_sse_server_mock_receive(stream_id: &str) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles.get_mut(stream_id).ok_or_else(|| {
            vm_error(format!(
                "sse_server_mock_receive: unknown stream '{stream_id}'"
            ))
        })?;
        if let Some(frame) = handle.frames.pop_front() {
            return Ok(sse_server_mock_frame_value(&frame));
        }
        if handle.closed || handle.cancelled || handle.disconnected {
            return Ok(sse_server_closed_event());
        }
        Ok(timeout_event())
    })
}

pub(super) fn vm_sse_server_mock_disconnect(stream_id: &str) -> Result<VmValue, VmError> {
    SSE_SERVER_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        let handle = handles.get_mut(stream_id).ok_or_else(|| {
            vm_error(format!(
                "sse_server_mock_disconnect: unknown stream '{stream_id}'"
            ))
        })?;
        if handle.disconnected {
            return Ok(VmValue::Bool(false));
        }
        handle.disconnected = true;
        handle.closed = true;
        Ok(VmValue::Bool(true))
    })
}

fn sse_server_mock_frame_value(frame: &str) -> VmValue {
    let mut event = MockStreamEvent {
        event_type: "message".to_string(),
        data: String::new(),
        id: None,
        retry_ms: None,
    };
    let mut data_lines = Vec::new();
    let mut comments = Vec::new();
    for raw in frame.lines() {
        if raw.is_empty() {
            continue;
        }
        if let Some(comment) = raw.strip_prefix(':') {
            comments.push(comment.strip_prefix(' ').unwrap_or(comment).to_string());
            continue;
        }
        let (field, value) = raw.split_once(':').unwrap_or((raw, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event.event_type = value.to_string(),
            "data" => data_lines.push(value.to_string()),
            "id" => event.id = Some(value.to_string()).filter(|value| !value.is_empty()),
            "retry" => event.retry_ms = value.parse::<i64>().ok(),
            _ => {}
        }
    }
    if !data_lines.is_empty() {
        event.data = data_lines.join("\n");
    }
    let mut value = if comments.is_empty() || !data_lines.is_empty() {
        sse_event_value(&event)
    } else {
        let mut dict = BTreeMap::new();
        dict.insert("type".to_string(), VmValue::String(Rc::from("comment")));
        dict.insert(
            "comment".to_string(),
            VmValue::String(Rc::from(comments.join("\n"))),
        );
        VmValue::Dict(Rc::new(dict))
    };
    if let VmValue::Dict(dict) = &mut value {
        let mut owned = (**dict).clone();
        owned.insert("raw".to_string(), VmValue::String(Rc::from(frame)));
        value = VmValue::Dict(Rc::new(owned));
    }
    value
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

pub(super) async fn vm_sse_connect(
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
    crate::egress::enforce_url_allowed("sse_connect", url).await?;

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

pub(super) async fn vm_sse_receive(stream_id: &str, timeout_ms: u64) -> Result<VmValue, VmError> {
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

pub(super) fn register_http_streaming_builtins(vm: &mut Vm) {
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

    vm.register_builtin("sse_event", |args, _out| {
        let Some(event) = args.first() else {
            return Err(vm_error("sse_event: requires event data or an event dict"));
        };
        let options = get_options_arg(args, 1);
        Ok(VmValue::String(Rc::from(vm_sse_event_frame(
            event, &options,
        )?)))
    });

    vm.register_builtin("sse_server_response", |args, _out| {
        let options = get_options_arg(args, 0);
        vm_sse_server_response(&options)
    });

    vm.register_builtin("sse_server_send", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error("sse_server_send: requires stream and event"));
        }
        let stream_id = handle_from_value(&args[0], "sse_server_send")?;
        let options = get_options_arg(args, 2);
        vm_sse_server_send(&stream_id, &args[1], &options)
    });

    vm.register_builtin("sse_server_heartbeat", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_server_heartbeat: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_server_heartbeat")?;
        vm_sse_server_heartbeat(&stream_id, args.get(1))
    });

    vm.register_builtin("sse_server_flush", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_server_flush: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_server_flush")?;
        vm_sse_server_flush(&stream_id)
    });

    vm.register_builtin("sse_server_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_server_close: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_server_close")?;
        vm_sse_server_close(&stream_id)
    });

    vm.register_builtin("sse_server_cancel", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_server_cancel: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_server_cancel")?;
        vm_sse_server_cancel(&stream_id, args.get(1))
    });

    vm.register_builtin("sse_server_status", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_server_status: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_server_status")?;
        vm_sse_server_status(&stream_id)
    });

    vm.register_builtin("sse_server_disconnected", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error(
                "sse_server_disconnected: requires a stream handle",
            ));
        };
        let stream_id = handle_from_value(handle, "sse_server_disconnected")?;
        vm_sse_server_observed_bool(&stream_id, "sse_server_disconnected", |handle| {
            handle.disconnected
        })
    });

    vm.register_builtin("sse_server_cancelled", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("sse_server_cancelled: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "sse_server_cancelled")?;
        vm_sse_server_observed_bool(&stream_id, "sse_server_cancelled", |handle| {
            handle.cancelled
        })
    });

    vm.register_builtin("sse_server_mock_receive", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error(
                "sse_server_mock_receive: requires a stream handle",
            ));
        };
        let stream_id = handle_from_value(handle, "sse_server_mock_receive")?;
        vm_sse_server_mock_receive(&stream_id)
    });

    vm.register_builtin("sse_server_mock_disconnect", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error(
                "sse_server_mock_disconnect: requires a stream handle",
            ));
        };
        let stream_id = handle_from_value(handle, "sse_server_mock_disconnect")?;
        vm_sse_server_mock_disconnect(&stream_id)
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

    websocket::register_websocket_builtins(vm);

    vm.register_builtin("transport_mock_clear", |_args, _out| {
        clear_http_streams();
        SSE_MOCKS.with(|mocks| mocks.borrow_mut().clear());
        SSE_HANDLES.with(|handles| handles.borrow_mut().clear());
        WEBSOCKET_MOCKS.with(|mocks| mocks.borrow_mut().clear());
        WEBSOCKET_HANDLES.with(|handles| handles.borrow_mut().clear());
        WEBSOCKET_SERVERS.with(|servers| {
            let mut servers = servers.borrow_mut();
            for server in servers.values() {
                server.running.store(false, Ordering::SeqCst);
                let _ = TcpStream::connect(&server.addr);
            }
            servers.clear();
        });
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
