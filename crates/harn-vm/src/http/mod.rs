use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

mod client;
mod mock;
mod streaming;
#[cfg(test)]
mod tests;

use mock::{
    clear_http_mocks, http_mock_calls_value, parse_mock_responses, register_http_mock,
    reset_http_mocks,
};
pub use mock::{http_mock_calls_snapshot, push_http_mock, HttpMockCallSnapshot, HttpMockResponse};
#[cfg(test)]
use mock::{mock_call_headers_value, redact_mock_call_url};

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

pub(super) const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub(super) const DEFAULT_BACKOFF_MS: u64 = 1_000;
pub(super) const MAX_RETRY_DELAY_MS: u64 = 60_000;
pub(super) const DEFAULT_RETRYABLE_STATUSES: [u16; 6] = [408, 429, 500, 502, 503, 504];
pub(super) const DEFAULT_RETRYABLE_METHODS: [&str; 5] = ["GET", "HEAD", "PUT", "DELETE", "OPTIONS"];
pub(super) const DEFAULT_TRANSPORT_RECEIVE_TIMEOUT_MS: u64 = 30_000;
pub(super) const DEFAULT_MAX_STREAM_EVENTS: usize = 10_000;
pub(super) const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub(super) const DEFAULT_SERVER_MAX_BODY_BYTES: usize = 1024 * 1024;
pub(super) const DEFAULT_WEBSOCKET_SERVER_IDLE_TIMEOUT_MS: u64 = 30_000;
pub(super) const MAX_HTTP_SESSIONS: usize = 64;
pub(super) const MAX_HTTP_STREAMS: usize = 64;
pub(super) const MAX_SSE_STREAMS: usize = 64;
pub(super) const MAX_SSE_SERVER_STREAMS: usize = 64;
pub(super) const MAX_WEBSOCKETS: usize = 64;
pub(super) const MULTIPART_MOCK_BOUNDARY: &str = "harn-boundary";
pub(super) const MAX_HTTP_SERVERS: usize = 128;
pub(super) const MAX_WEBSOCKET_SERVERS: usize = 16;

thread_local! {
    static TRANSPORT_HANDLE_COUNTER: RefCell<u64> = const { RefCell::new(0) };
    static HTTP_SERVERS: RefCell<HashMap<String, HttpServer>> = RefCell::new(HashMap::new());
}

/// Reset thread-local HTTP mock state. Call between test runs.
pub fn reset_http_state() {
    reset_http_mocks();
    client::reset_client_state();
    streaming::reset_streaming_state();
    TRANSPORT_HANDLE_COUNTER.with(|counter| *counter.borrow_mut() = 0);
    HTTP_SERVERS.with(|servers| servers.borrow_mut().clear());
}

pub(super) fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

pub(super) fn next_transport_handle(prefix: &str) -> String {
    TRANSPORT_HANDLE_COUNTER.with(|counter| {
        let mut counter = counter.borrow_mut();
        *counter += 1;
        format!("{prefix}-{}", *counter)
    })
}

pub(super) fn handle_from_value(value: &VmValue, builtin: &str) -> Result<String, VmError> {
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

pub(super) fn get_options_arg(args: &[VmValue], index: usize) -> BTreeMap<String, VmValue> {
    args.get(index)
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default()
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
    register_http_tls_builtins(vm);
    client::register_http_verb_builtins(vm);
    register_http_server_builtins(vm);
    register_http_mock_builtins(vm);
    client::register_http_client_builtins(vm);
    streaming::register_http_streaming_builtins(vm);
}

fn register_http_tls_builtins(vm: &mut Vm) {
    vm.register_builtin("http_server_tls_plain", |_args, _out| {
        Ok(http_server_tls_config_value(
            "plain",
            false,
            "http",
            false,
            BTreeMap::new(),
        ))
    });
    vm.register_builtin("http_server_tls_edge", |args, _out| {
        let options = get_options_arg(args, 0);
        Ok(http_server_tls_config_value(
            "edge",
            false,
            "https",
            vm_get_bool_option(&options, "hsts", true),
            hsts_options(&options),
        ))
    });
    vm.register_builtin("http_server_tls_pem", |args, _out| {
        if args.len() < 2 {
            return Err(vm_error(
                "http_server_tls_pem: requires cert path and key path",
            ));
        }
        let cert_path = args[0].display();
        let key_path = args[1].display();
        if !std::path::Path::new(&cert_path).is_file() {
            return Err(vm_error(format!(
                "http_server_tls_pem: certificate not found: {cert_path}"
            )));
        }
        if !std::path::Path::new(&key_path).is_file() {
            return Err(vm_error(format!(
                "http_server_tls_pem: private key not found: {key_path}"
            )));
        }
        let mut extra = BTreeMap::new();
        extra.insert(
            "cert_path".to_string(),
            VmValue::String(Rc::from(cert_path)),
        );
        extra.insert("key_path".to_string(), VmValue::String(Rc::from(key_path)));
        Ok(http_server_tls_config_value(
            "pem", true, "https", true, extra,
        ))
    });
    vm.register_builtin("http_server_tls_self_signed_dev", |args, _out| {
        let hosts = tls_hosts_arg(args.first())?;
        let cert = rcgen::generate_simple_self_signed(hosts.clone()).map_err(|error| {
            vm_error(format!(
                "http_server_tls_self_signed_dev: failed to generate certificate: {error}"
            ))
        })?;
        let mut extra = BTreeMap::new();
        extra.insert(
            "hosts".to_string(),
            VmValue::List(Rc::new(
                hosts
                    .into_iter()
                    .map(|host| VmValue::String(Rc::from(host)))
                    .collect(),
            )),
        );
        extra.insert(
            "cert_pem".to_string(),
            VmValue::String(Rc::from(cert.cert.pem())),
        );
        extra.insert(
            "key_pem".to_string(),
            VmValue::String(Rc::from(cert.key_pair.serialize_pem())),
        );
        Ok(http_server_tls_config_value(
            "self_signed_dev",
            true,
            "https",
            false,
            extra,
        ))
    });
    vm.register_builtin("http_server_security_headers", |args, _out| {
        let Some(VmValue::Dict(config)) = args.first() else {
            return Err(vm_error(
                "http_server_security_headers: requires a TLS config dict",
            ));
        };
        Ok(VmValue::Dict(Rc::new(http_server_security_headers(config))))
    });
}

fn register_http_server_builtins(vm: &mut Vm) {
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
}

fn register_http_mock_builtins(vm: &mut Vm) {
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

        register_http_mock(method, url_pattern, responses);
        Ok(VmValue::Nil)
    });

    // http_mock_clear() -> nil
    vm.register_builtin("http_mock_clear", |_args, _out| {
        clear_http_mocks();
        client::clear_http_streams();
        Ok(VmValue::Nil)
    });

    // http_mock_calls(options?) -> list of {method, url, headers, body}
    vm.register_builtin("http_mock_calls", |args, _out| {
        let options = get_options_arg(args, 0);
        let include_sensitive = get_bool_option(&options, "include_sensitive", false)
            || get_bool_option(&options, "include_sensitive_headers", false);
        let redact_sensitive = get_bool_option(
            &options,
            "redact_sensitive",
            get_bool_option(&options, "redact_headers", true),
        ) && !include_sensitive;
        Ok(VmValue::List(Rc::new(http_mock_calls_value(
            redact_sensitive,
        ))))
    });
}

fn http_server_tls_config_value(
    mode: &str,
    terminate_tls: bool,
    scheme: &str,
    hsts: bool,
    extra: BTreeMap<String, VmValue>,
) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("mode".to_string(), VmValue::String(Rc::from(mode)));
    dict.insert("terminate_tls".to_string(), VmValue::Bool(terminate_tls));
    dict.insert("scheme".to_string(), VmValue::String(Rc::from(scheme)));
    dict.insert("hsts".to_string(), VmValue::Bool(hsts));
    for (key, value) in extra {
        dict.insert(key, value);
    }
    VmValue::Dict(Rc::new(dict))
}

fn hsts_options(options: &BTreeMap<String, VmValue>) -> BTreeMap<String, VmValue> {
    let mut hsts = BTreeMap::new();
    hsts.insert(
        "hsts_max_age_seconds".to_string(),
        VmValue::Int(vm_get_int_option(
            options,
            "hsts_max_age_seconds",
            31_536_000,
        )),
    );
    hsts.insert(
        "hsts_include_subdomains".to_string(),
        VmValue::Bool(vm_get_bool_option(
            options,
            "hsts_include_subdomains",
            false,
        )),
    );
    hsts.insert(
        "hsts_preload".to_string(),
        VmValue::Bool(vm_get_bool_option(options, "hsts_preload", false)),
    );
    hsts
}

fn http_server_security_headers(config: &BTreeMap<String, VmValue>) -> BTreeMap<String, VmValue> {
    let hsts_enabled = vm_get_bool_option(config, "hsts", false);
    if !hsts_enabled {
        return BTreeMap::new();
    }
    let mut value = format!(
        "max-age={}",
        vm_get_int_option(config, "hsts_max_age_seconds", 31_536_000).max(0)
    );
    if vm_get_bool_option(config, "hsts_include_subdomains", false) {
        value.push_str("; includeSubDomains");
    }
    if vm_get_bool_option(config, "hsts_preload", false) {
        value.push_str("; preload");
    }
    BTreeMap::from([(
        "strict-transport-security".to_string(),
        VmValue::String(Rc::from(value)),
    )])
}

fn tls_hosts_arg(value: Option<&VmValue>) -> Result<Vec<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(vec!["localhost".to_string(), "127.0.0.1".to_string()]),
        Some(VmValue::List(hosts)) => {
            let mut parsed = Vec::new();
            for host in hosts.iter() {
                let host = host.display();
                if host.is_empty() {
                    return Err(vm_error(
                        "http_server_tls_self_signed_dev: host names must be non-empty",
                    ));
                }
                parsed.push(host);
            }
            if parsed.is_empty() {
                return Err(vm_error(
                    "http_server_tls_self_signed_dev: host list must not be empty",
                ));
            }
            Ok(parsed)
        }
        Some(other) => {
            let host = other.display();
            if host.is_empty() {
                return Err(vm_error(
                    "http_server_tls_self_signed_dev: host name must be non-empty",
                ));
            }
            Ok(vec![host])
        }
    }
}

pub(super) fn vm_get_int_option(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    default: i64,
) -> i64 {
    options.get(key).and_then(|v| v.as_int()).unwrap_or(default)
}

pub(super) fn vm_get_bool_option(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    default: bool,
) -> bool {
    match options.get(key) {
        Some(VmValue::Bool(b)) => *b,
        _ => default,
    }
}

pub(super) fn vm_get_int_option_prefer(
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

pub(super) fn vm_get_optional_int_option(
    options: &BTreeMap<String, VmValue>,
    key: &str,
) -> Option<u64> {
    options
        .get(key)
        .and_then(|value| value.as_int())
        .map(|value| value.max(0) as u64)
}

pub(super) fn string_option(options: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    options
        .get(key)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
}
