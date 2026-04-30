use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, RwLock,
};
use std::thread;
use std::time::Duration;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::super::mock::url_matches;
use super::super::{
    get_options_arg, handle_from_value, next_transport_handle, vm_error, vm_get_int_option,
    vm_get_int_option_prefer, DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_MAX_STREAM_EVENTS,
    DEFAULT_TIMEOUT_MS, DEFAULT_WEBSOCKET_SERVER_IDLE_TIMEOUT_MS, MAX_WEBSOCKETS,
    MAX_WEBSOCKET_SERVERS,
};
use super::{
    closed_event, receive_timeout_arg, record_transport_call, timeout_event,
    transport_limit_option, FakeWebSocket, MockWsMessage, ServerWebSocket, ServerWebSocketCommand,
    TransportMockCall, WebSocketHandle, WebSocketHandleKind, WebSocketMock, WebSocketRoute,
    WebSocketServer, WebSocketServerEvent, WEBSOCKET_HANDLES, WEBSOCKET_MOCKS, WEBSOCKET_SERVERS,
};

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
            MockWsMessage {
                message_type,
                data,
                close_code: dict
                    .get("code")
                    .and_then(|value| value.as_int())
                    .map(|value| value as u16),
                close_reason: dict.get("reason").map(|value| value.display()),
            }
        }
        VmValue::Bytes(bytes) => MockWsMessage {
            message_type: "binary".to_string(),
            data: bytes.as_ref().clone(),
            close_code: None,
            close_reason: None,
        },
        other => MockWsMessage {
            message_type: "text".to_string(),
            data: other.display().into_bytes(),
            close_code: None,
            close_reason: None,
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

fn closed_event_with(code: Option<u16>, reason: Option<String>) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("type".to_string(), VmValue::String(Rc::from("close")));
    if let Some(code) = code {
        dict.insert("code".to_string(), VmValue::Int(i64::from(code)));
    }
    if let Some(reason) = reason {
        dict.insert("reason".to_string(), VmValue::String(Rc::from(reason)));
    }
    VmValue::Dict(Rc::new(dict))
}

fn ws_event_value(message: MockWsMessage) -> VmValue {
    if message.message_type == "close" {
        return closed_event_with(message.close_code, message.close_reason);
    }
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
            close_code: None,
            close_reason: None,
        }),
        WsMessage::Binary(bytes) => ws_event_value(MockWsMessage {
            message_type: "binary".to_string(),
            data: bytes.to_vec(),
            close_code: None,
            close_reason: None,
        }),
        WsMessage::Ping(bytes) => ws_event_value(MockWsMessage {
            message_type: "ping".to_string(),
            data: bytes.to_vec(),
            close_code: None,
            close_reason: None,
        }),
        WsMessage::Pong(bytes) => ws_event_value(MockWsMessage {
            message_type: "pong".to_string(),
            data: bytes.to_vec(),
            close_code: None,
            close_reason: None,
        }),
        WsMessage::Close(frame) => match frame {
            Some(frame) => {
                closed_event_with(Some(u16::from(frame.code)), Some(frame.reason.to_string()))
            }
            None => closed_event(),
        },
        WsMessage::Frame(_) => VmValue::Nil,
    }
}

fn websocket_route_from_options(path: &str, options: &BTreeMap<String, VmValue>) -> WebSocketRoute {
    let bearer_token = options.get("auth").and_then(|auth| match auth {
        VmValue::Dict(dict) => dict.get("bearer").map(|value| value.display()),
        other => {
            let value = other.display();
            (!value.is_empty()).then_some(value)
        }
    });
    WebSocketRoute {
        path: path.to_string(),
        bearer_token,
        max_messages: transport_limit_option(options, "max_messages", DEFAULT_MAX_STREAM_EVENTS)
            .max(1),
        max_message_bytes: transport_limit_option(
            options,
            "max_message_bytes",
            DEFAULT_MAX_MESSAGE_BYTES,
        )
        .max(1),
        send_buffer_messages: transport_limit_option(options, "send_buffer_messages", 64),
        idle_timeout_ms: vm_get_int_option(
            options,
            "idle_timeout_ms",
            DEFAULT_WEBSOCKET_SERVER_IDLE_TIMEOUT_MS as i64,
        )
        .max(0) as u64,
    }
}

pub(super) fn vm_websocket_server(
    bind: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let listener = TcpListener::bind(bind)
        .map_err(|error| vm_error(format!("websocket_server: bind failed: {error}")))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| vm_error(format!("websocket_server: nonblocking failed: {error}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| vm_error(format!("websocket_server: local addr failed: {error}")))?;
    let id = next_transport_handle("websocket-server");
    let addr = local_addr.to_string();
    let url = format!("ws://{addr}");
    let routes = Arc::new(RwLock::new(HashMap::<String, WebSocketRoute>::new()));
    if let Some(path) = options
        .get("path")
        .map(|value| value.display())
        .filter(|path| !path.is_empty())
    {
        if !path.starts_with('/') {
            return Err(vm_error("websocket_server: path must start with '/'"));
        }
        routes
            .write()
            .expect("websocket routes poisoned")
            .insert(path.clone(), websocket_route_from_options(&path, options));
    }
    let (event_tx, event_rx) = mpsc::channel();
    let running = Arc::new(AtomicBool::new(true));
    let server_routes = routes.clone();
    let server_running = running.clone();
    thread::Builder::new()
        .name(format!("harn-ws-{id}"))
        .spawn(move || websocket_server_loop(listener, server_routes, event_tx, server_running))
        .map_err(|error| vm_error(format!("websocket_server: spawn failed: {error}")))?;
    WEBSOCKET_SERVERS.with(|servers| {
        let mut servers = servers.borrow_mut();
        if servers.len() >= MAX_WEBSOCKET_SERVERS {
            running.store(false, Ordering::SeqCst);
            let _ = TcpStream::connect(&addr);
            return Err(vm_error(format!(
                "websocket_server: maximum open servers ({MAX_WEBSOCKET_SERVERS}) reached"
            )));
        }
        servers.insert(
            id.clone(),
            WebSocketServer {
                addr: addr.clone(),
                routes,
                events: Rc::new(tokio::sync::Mutex::new(event_rx)),
                running,
            },
        );
        Ok(())
    })?;
    let mut dict = BTreeMap::new();
    dict.insert("id".to_string(), VmValue::String(Rc::from(id)));
    dict.insert("addr".to_string(), VmValue::String(Rc::from(addr)));
    dict.insert("url".to_string(), VmValue::String(Rc::from(url)));
    Ok(VmValue::Dict(Rc::new(dict)))
}

pub(super) fn vm_websocket_route(
    server_id: &str,
    path: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let routes = WEBSOCKET_SERVERS.with(|servers| {
        servers
            .borrow()
            .get(server_id)
            .map(|server| server.routes.clone())
    });
    let Some(routes) = routes else {
        return Err(vm_error(format!(
            "websocket_route: unknown server '{server_id}'"
        )));
    };
    routes.write().expect("websocket routes poisoned").insert(
        path.to_string(),
        websocket_route_from_options(path, options),
    );
    Ok(VmValue::Bool(true))
}

pub(super) async fn vm_websocket_accept(
    server_id: &str,
    timeout_ms: u64,
) -> Result<VmValue, VmError> {
    let receiver = WEBSOCKET_SERVERS.with(|servers| {
        servers
            .borrow()
            .get(server_id)
            .map(|server| server.events.clone())
    });
    let Some(receiver) = receiver else {
        return Err(vm_error(format!(
            "websocket_accept: unknown server '{server_id}'"
        )));
    };
    let started = std::time::Instant::now();
    loop {
        let event = {
            let receiver = receiver.lock().await;
            receiver.try_recv()
        };
        match event {
            Ok(event) => return register_accepted_websocket(event),
            Err(mpsc::TryRecvError::Disconnected) => return Ok(VmValue::Nil),
            Err(mpsc::TryRecvError::Empty) => {
                if timeout_ms == 0 || started.elapsed() >= Duration::from_millis(timeout_ms) {
                    return Ok(timeout_event());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

fn register_accepted_websocket(event: WebSocketServerEvent) -> Result<VmValue, VmError> {
    let WebSocketServerEvent {
        handle,
        path,
        peer,
        headers,
        max_messages,
        max_message_bytes,
    } = event;
    let id = next_transport_handle("websocket");
    WEBSOCKET_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        if handles.len() >= MAX_WEBSOCKETS {
            return Err(vm_error(format!(
                "websocket_accept: maximum open sockets ({MAX_WEBSOCKETS}) reached"
            )));
        }
        handles.insert(
            id.clone(),
            WebSocketHandle {
                kind: WebSocketHandleKind::Server(Rc::new(tokio::sync::Mutex::new(handle))),
                url: path.clone(),
                max_messages,
                max_message_bytes,
                received: 0,
            },
        );
        Ok(())
    })?;
    let mut metadata = BTreeMap::new();
    metadata.insert("id".to_string(), VmValue::String(Rc::from(id)));
    metadata.insert("path".to_string(), VmValue::String(Rc::from(path)));
    metadata.insert("peer".to_string(), VmValue::String(Rc::from(peer)));
    metadata.insert(
        "headers".to_string(),
        VmValue::Dict(Rc::new(
            headers
                .into_iter()
                .map(|(name, value)| (name, VmValue::String(Rc::from(value))))
                .collect(),
        )),
    );
    Ok(VmValue::Dict(Rc::new(metadata)))
}

pub(super) fn vm_websocket_server_close(server_id: &str) -> Result<VmValue, VmError> {
    let server = WEBSOCKET_SERVERS.with(|servers| servers.borrow_mut().remove(server_id));
    let Some(server) = server else {
        return Ok(VmValue::Bool(false));
    };
    server.running.store(false, Ordering::SeqCst);
    let _ = TcpStream::connect(&server.addr);
    Ok(VmValue::Bool(true))
}

fn websocket_server_loop(
    listener: TcpListener,
    routes: Arc<RwLock<HashMap<String, WebSocketRoute>>>,
    event_tx: mpsc::Sender<WebSocketServerEvent>,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let routes = routes.clone();
                let event_tx = event_tx.clone();
                let running = running.clone();
                let peer = peer.to_string();
                let _ = thread::Builder::new()
                    .name("harn-ws-conn".to_string())
                    .spawn(move || {
                        websocket_connection_thread(stream, peer, routes, event_tx, running);
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

fn error_response(status: StatusCode, body: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(body.to_string()));
    *response.status_mut() = status;
    response
}

fn route_for_request(
    request: &Request,
    routes: &Arc<RwLock<HashMap<String, WebSocketRoute>>>,
) -> Result<WebSocketRoute, ErrorResponse> {
    let path = request.uri().path().to_string();
    let Some(route) = routes
        .read()
        .expect("websocket routes poisoned")
        .get(&path)
        .cloned()
    else {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "websocket route not found",
        ));
    };
    if let Some(token) = route.bearer_token.as_ref() {
        let expected = format!("Bearer {token}");
        let authorized = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(|value| value == expected)
            .unwrap_or(false);
        if !authorized {
            return Err(error_response(
                StatusCode::UNAUTHORIZED,
                "websocket route unauthorized",
            ));
        }
    }
    Ok(route)
}

fn websocket_connection_thread(
    stream: TcpStream,
    peer: String,
    routes: Arc<RwLock<HashMap<String, WebSocketRoute>>>,
    event_tx: mpsc::Sender<WebSocketServerEvent>,
    running: Arc<AtomicBool>,
) {
    let accepted_route = Arc::new(std::sync::Mutex::new(
        None::<(WebSocketRoute, BTreeMap<String, String>)>,
    ));
    let callback_route = accepted_route.clone();
    let callback =
        move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
            let route = route_for_request(request, &routes)?;
            let headers = request
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
                })
                .collect::<BTreeMap<_, _>>();
            *callback_route
                .lock()
                .expect("websocket route metadata poisoned") = Some((route, headers));
            Ok(response)
        };
    let Ok(mut socket) = tokio_tungstenite::tungstenite::accept_hdr(stream, callback) else {
        return;
    };
    let Some((route, headers)) = accepted_route
        .lock()
        .expect("websocket route metadata poisoned")
        .clone()
    else {
        let _ = socket.close(None);
        return;
    };
    let _ = socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(50)));
    let (incoming_tx, incoming_rx) = mpsc::channel();
    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel(route.send_buffer_messages);
    let event = WebSocketServerEvent {
        handle: ServerWebSocket {
            incoming: VecDeque::new(),
            incoming_rx,
            outgoing_tx,
            closed: false,
        },
        path: route.path.clone(),
        peer,
        headers,
        max_messages: route.max_messages,
        max_message_bytes: route.max_message_bytes,
    };
    if event_tx.send(event).is_err() {
        let _ = socket.close(None);
        return;
    }
    let mut last_activity = std::time::Instant::now();
    while running.load(Ordering::SeqCst) {
        while let Ok(command) = outgoing_rx.try_recv() {
            match command {
                ServerWebSocketCommand::Send(message) => {
                    let Ok(message) = real_ws_message(&message) else {
                        continue;
                    };
                    if socket.send(message).is_err() {
                        return;
                    }
                    last_activity = std::time::Instant::now();
                }
                ServerWebSocketCommand::Close(code, reason) => {
                    let _ = socket.close(close_frame(code, reason));
                    return;
                }
            }
        }
        if route.idle_timeout_ms > 0
            && last_activity.elapsed() >= Duration::from_millis(route.idle_timeout_ms)
        {
            let _ = socket.close(close_frame(Some(1001), Some("idle timeout".to_string())));
            let _ = incoming_tx.send(MockWsMessage {
                message_type: "close".to_string(),
                data: Vec::new(),
                close_code: Some(1001),
                close_reason: Some("idle timeout".to_string()),
            });
            return;
        }
        match socket.read() {
            Ok(message) => {
                last_activity = std::time::Instant::now();
                if incoming_tx
                    .send(mock_ws_message_from_real(message))
                    .is_err()
                {
                    return;
                }
            }
            Err(tokio_tungstenite::tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => {
                let _ = incoming_tx.send(MockWsMessage {
                    message_type: "close".to_string(),
                    data: Vec::new(),
                    close_code: None,
                    close_reason: None,
                });
                return;
            }
        }
    }
    let _ = socket.get_mut().shutdown(Shutdown::Both);
}

pub(super) async fn vm_websocket_connect(
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
    crate::egress::enforce_url_allowed("websocket_connect", url).await?;
    let timeout_ms = vm_get_int_option_prefer(
        options,
        "timeout_ms",
        "timeout",
        DEFAULT_TIMEOUT_MS as i64,
    )
    .max(0) as u64;
    let request = websocket_client_request(url, options)?;
    let connect = tokio_tungstenite::connect_async(request);
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
    Ok(MockWsMessage {
        message_type,
        data,
        close_code: options
            .get("code")
            .or_else(|| options.get("close_code"))
            .and_then(|value| value.as_int())
            .map(|value| value as u16),
        close_reason: options
            .get("reason")
            .or_else(|| options.get("close_reason"))
            .map(|value| value.display()),
    })
}

fn websocket_client_request(
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, VmError> {
    let mut request = url
        .into_client_request()
        .map_err(|error| vm_error(format!("websocket_connect: invalid request: {error}")))?;
    if let Some(headers) = options.get("headers").and_then(|value| value.as_dict()) {
        for (name, value) in headers {
            let header_name = tokio_tungstenite::tungstenite::http::header::HeaderName::from_bytes(
                name.as_bytes(),
            )
            .map_err(|error| {
                vm_error(format!(
                    "websocket_connect: invalid header name '{name}': {error}"
                ))
            })?;
            let header_value = HeaderValue::from_str(&value.display()).map_err(|error| {
                vm_error(format!(
                    "websocket_connect: invalid header value for '{name}': {error}"
                ))
            })?;
            request.headers_mut().insert(header_name, header_value);
        }
    }
    if let Some(auth) = options.get("auth") {
        let bearer = match auth {
            VmValue::Dict(dict) => dict.get("bearer").map(|value| value.display()),
            other => Some(other.display()),
        };
        if let Some(token) = bearer.filter(|token| !token.is_empty()) {
            let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| {
                vm_error(format!(
                    "websocket_connect: invalid authorization header: {error}"
                ))
            })?;
            request.headers_mut().insert("authorization", value);
        }
    }
    Ok(request)
}

fn close_frame(code: Option<u16>, reason: Option<String>) -> Option<CloseFrame> {
    code.or(reason.as_ref().map(|_| 1000))
        .map(|code| CloseFrame {
            code: CloseCode::from(code),
            reason: reason.unwrap_or_default().into(),
        })
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
        "close" => Ok(WsMessage::Close(close_frame(
            message.close_code,
            message.close_reason.clone(),
        ))),
        other => Err(vm_error(format!(
            "websocket_send: unsupported message type '{other}'"
        ))),
    }
}

fn mock_ws_message_from_real(message: WsMessage) -> MockWsMessage {
    match message {
        WsMessage::Text(text) => MockWsMessage {
            message_type: "text".to_string(),
            data: text.as_bytes().to_vec(),
            close_code: None,
            close_reason: None,
        },
        WsMessage::Binary(bytes) => MockWsMessage {
            message_type: "binary".to_string(),
            data: bytes.to_vec(),
            close_code: None,
            close_reason: None,
        },
        WsMessage::Ping(bytes) => MockWsMessage {
            message_type: "ping".to_string(),
            data: bytes.to_vec(),
            close_code: None,
            close_reason: None,
        },
        WsMessage::Pong(bytes) => MockWsMessage {
            message_type: "pong".to_string(),
            data: bytes.to_vec(),
            close_code: None,
            close_reason: None,
        },
        WsMessage::Close(frame) => {
            let (close_code, close_reason) = frame
                .map(|frame| (Some(u16::from(frame.code)), Some(frame.reason.to_string())))
                .unwrap_or((None, None));
            MockWsMessage {
                message_type: "close".to_string(),
                data: Vec::new(),
                close_code,
                close_reason,
            }
        }
        WsMessage::Frame(_) => MockWsMessage {
            message_type: "close".to_string(),
            data: Vec::new(),
            close_code: None,
            close_reason: None,
        },
    }
}

pub(super) async fn vm_websocket_send(
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
            WebSocketHandleKind::Server(socket) => WebSocketHandleKind::Server(socket.clone()),
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
        WebSocketHandleKind::Server(socket) => {
            let mut socket = socket.lock().await;
            if socket.closed {
                return Ok(VmValue::Bool(false));
            }
            let command = if message.message_type == "close" {
                socket.closed = true;
                ServerWebSocketCommand::Close(message.close_code, message.close_reason.clone())
            } else {
                ServerWebSocketCommand::Send(message.clone())
            };
            socket
                .outgoing_tx
                .try_send(command)
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => vm_error("websocket_send: send buffer full"),
                    mpsc::TrySendError::Disconnected(_) => {
                        vm_error("websocket_send: connection closed")
                    }
                })?;
            Ok(VmValue::Bool(true))
        }
    }
}

pub(super) async fn vm_websocket_receive(
    socket_id: &str,
    timeout_ms: u64,
) -> Result<VmValue, VmError> {
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
            WebSocketHandleKind::Server(socket) => WebSocketHandleKind::Server(socket.clone()),
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
        WebSocketHandleKind::Server(socket) => {
            let started = std::time::Instant::now();
            loop {
                let message = {
                    let mut socket = socket.lock().await;
                    if socket.closed {
                        return Ok(VmValue::Nil);
                    }
                    while let Ok(message) = socket.incoming_rx.try_recv() {
                        socket.incoming.push_back(message);
                    }
                    socket.incoming.pop_front()
                };
                if let Some(message) = message {
                    if message.data.len() > max_message_bytes {
                        let mut socket = socket.lock().await;
                        socket.closed = true;
                        let _ = socket.outgoing_tx.try_send(ServerWebSocketCommand::Close(
                            Some(1009),
                            Some("message too large".to_string()),
                        ));
                        return Err(vm_error(format!(
                            "websocket_receive: message exceeded max_message_bytes ({max_message_bytes})"
                        )));
                    }
                    if message.message_type == "close" {
                        let mut socket = socket.lock().await;
                        socket.closed = true;
                    }
                    return Ok(ws_event_value(message));
                }
                if timeout_ms == 0 || started.elapsed() >= Duration::from_millis(timeout_ms) {
                    return Ok(timeout_event());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

pub(super) async fn vm_websocket_close(socket_id: &str) -> Result<VmValue, VmError> {
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
        WebSocketHandleKind::Server(socket) => {
            let mut socket = socket.lock().await;
            socket.closed = true;
            let _ = socket
                .outgoing_tx
                .try_send(ServerWebSocketCommand::Close(None, None));
            Ok(VmValue::Bool(true))
        }
    }
}

pub(super) fn register_websocket_builtins(vm: &mut Vm) {
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

    vm.register_builtin("websocket_server", |args, _out| {
        let bind = args
            .first()
            .map(|arg| arg.display())
            .filter(|bind| !bind.is_empty())
            .unwrap_or_else(|| "127.0.0.1:0".to_string());
        let options = get_options_arg(args, 1);
        vm_websocket_server(&bind, &options)
    });

    vm.register_builtin("websocket_route", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error(
                "websocket_route: requires server handle and route path",
            ));
        }
        let server_id = handle_from_value(&args[0], "websocket_route")?;
        let path = args[1].display();
        if path.is_empty() || !path.starts_with('/') {
            return Err(vm_error("websocket_route: path must start with '/'"));
        }
        let options = get_options_arg(args, 2);
        vm_websocket_route(&server_id, &path, &options)
    });

    vm.register_async_builtin("websocket_accept", |args| async move {
        let Some(handle) = args.first() else {
            return Err(vm_error("websocket_accept: requires a server handle"));
        };
        let server_id = handle_from_value(handle, "websocket_accept")?;
        let timeout_ms = receive_timeout_arg(&args, 1);
        vm_websocket_accept(&server_id, timeout_ms).await
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

    vm.register_builtin("websocket_server_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("websocket_server_close: requires a server handle"));
        };
        let server_id = handle_from_value(handle, "websocket_server_close")?;
        vm_websocket_server_close(&server_id)
    });
}
