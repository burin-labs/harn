//! HTTP response codec primitives for `.harn` HTTP handlers.
//!
//! Handlers hosted on `harn-serve` build their replies with these
//! builtins instead of bare JSON dicts. The codec on the server side
//! (`harn_serve::http_codec`) recognises the tagged record, lifts
//! status/headers/body out of it, and renders it as a proper HTTP
//! response — JSON, no-content, error envelope, buffered stream, or
//! Server-Sent Events.
//!
//! The tag is the literal string `"v1"` under the
//! `__http_response__` key. Plain dicts that happen to define
//! `status`/`headers`/`body` are not picked up by the codec; only the
//! tagged record is.
//!
//! Channel-bearing bodies (`http_stream(channel)`, `http_sse(channel)`)
//! are drained inside the builtin before the handler returns. This
//! keeps the v1 codec wire-format JSON-only — the channel is
//! materialised into a list of chunks/events. Authors write the same
//! code that true streaming will accept; only the on-the-wire timing
//! differs.

use std::collections::BTreeMap;
use std::rc::Rc;

use sha2::{Digest, Sha256};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Tag key + version that the harn-serve codec keys off of.
pub const HTTP_RESPONSE_TAG_KEY: &str = "__http_response__";
pub const HTTP_RESPONSE_TAG_VERSION: &str = "v1";

const BODY_KIND_JSON: &str = "json";
const BODY_KIND_NONE: &str = "none";
const BODY_KIND_STREAM: &str = "stream";
const BODY_KIND_SSE: &str = "sse";

pub(crate) fn register_http_response_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "http_ok(body: any?) -> dict", category = "http_response")]
fn http_ok_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let body = args.first().cloned().unwrap_or(VmValue::Nil);
    Ok(envelope(200, body, BODY_KIND_JSON, BTreeMap::new()))
}

#[harn_builtin(
    sig = "http_created(body: any?, location?: string?) -> dict",
    category = "http_response"
)]
fn http_created_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let body = args.first().cloned().unwrap_or(VmValue::Nil);
    let mut headers = BTreeMap::new();
    if let Some(location) = args.get(1).and_then(string_or_nil) {
        headers.insert("Location".to_string(), VmValue::String(Rc::from(location)));
    }
    Ok(envelope(201, body, BODY_KIND_JSON, headers))
}

#[harn_builtin(sig = "http_no_content() -> dict", category = "http_response")]
fn http_no_content_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(envelope(204, VmValue::Nil, BODY_KIND_NONE, BTreeMap::new()))
}

#[harn_builtin(
    sig = "http_error(status: int, code: string, message: string, details?: any) -> dict",
    category = "http_response"
)]
fn http_error_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let status = require_status(args.first(), "http_error")?;
    if !(400..=599).contains(&status) {
        return Err(thrown_err(format!(
            "http_error: status must be 4xx or 5xx (got {status})"
        )));
    }
    let code = require_nonempty_string(args.get(1), "http_error", "code")?;
    let message = require_nonempty_string(args.get(2), "http_error", "message")?;
    let details = args.get(3).cloned().unwrap_or(VmValue::Nil);

    let mut body = BTreeMap::new();
    body.insert("code".to_string(), VmValue::String(Rc::from(code)));
    body.insert("message".to_string(), VmValue::String(Rc::from(message)));
    if !matches!(details, VmValue::Nil) {
        body.insert("details".to_string(), details);
    }
    let mut env = envelope_map(
        status,
        VmValue::Dict(Rc::new(body)),
        BODY_KIND_JSON,
        BTreeMap::new(),
    );
    env.insert("is_error".to_string(), VmValue::Bool(true));
    Ok(VmValue::Dict(Rc::new(env)))
}

#[harn_builtin(
    sig = "http_reply(status: int, body?: any, headers?: dict) -> dict",
    category = "http_response"
)]
fn http_reply_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let status = require_status(args.first(), "http_reply")?;
    let body = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let headers = parse_headers(args.get(2), "http_reply")?;
    let body_kind = if status == 204 || status == 304 || matches!(body, VmValue::Nil) {
        BODY_KIND_NONE
    } else {
        BODY_KIND_JSON
    };
    let body_for_envelope = if body_kind == BODY_KIND_NONE {
        VmValue::Nil
    } else {
        body
    };
    Ok(envelope(status, body_for_envelope, body_kind, headers))
}

#[harn_builtin(
    sig = "http_stream(source: any, content_type?: string?) -> dict",
    kind = "async",
    category = "http_response"
)]
async fn http_stream_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let source = args
        .first()
        .cloned()
        .ok_or_else(|| thrown_err("http_stream: source is required"))?;
    let content_type = args
        .get(1)
        .and_then(string_or_nil)
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let chunks = drain_to_list(source, "http_stream").await?;
    let mut headers = BTreeMap::new();
    headers.insert(
        "Content-Type".to_string(),
        VmValue::String(Rc::from(content_type)),
    );
    Ok(envelope(
        200,
        VmValue::List(Rc::new(chunks)),
        BODY_KIND_STREAM,
        headers,
    ))
}

#[harn_builtin(
    sig = "http_sse(source: any, retry_ms?: int?) -> dict",
    kind = "async",
    category = "http_response"
)]
async fn http_sse_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let source = args
        .first()
        .cloned()
        .ok_or_else(|| thrown_err("http_sse: source is required"))?;
    let retry_ms = match args.get(1) {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Int(value)) => {
            if *value < 0 {
                return Err(thrown_err(format!(
                    "http_sse: retry_ms must be non-negative (got {value})"
                )));
            }
            Some(*value)
        }
        Some(other) => {
            return Err(thrown_err(format!(
                "http_sse: retry_ms must be an integer (got {})",
                other.type_name()
            )));
        }
    };

    let events = drain_to_list(source, "http_sse").await?;
    let mut headers = BTreeMap::new();
    headers.insert(
        "Content-Type".to_string(),
        VmValue::String(Rc::from("text/event-stream")),
    );
    headers.insert(
        "Cache-Control".to_string(),
        VmValue::String(Rc::from("no-cache")),
    );
    let mut env = envelope_map(200, VmValue::List(Rc::new(events)), BODY_KIND_SSE, headers);
    if let Some(retry_ms) = retry_ms {
        env.insert("retry_ms".to_string(), VmValue::Int(retry_ms));
    }
    Ok(VmValue::Dict(Rc::new(env)))
}

fn envelope(
    status: i64,
    body: VmValue,
    body_kind: &str,
    headers: BTreeMap<String, VmValue>,
) -> VmValue {
    VmValue::Dict(Rc::new(envelope_map(status, body, body_kind, headers)))
}

fn envelope_map(
    status: i64,
    body: VmValue,
    body_kind: &str,
    headers: BTreeMap<String, VmValue>,
) -> BTreeMap<String, VmValue> {
    let mut map = BTreeMap::new();
    map.insert(
        HTTP_RESPONSE_TAG_KEY.to_string(),
        VmValue::String(Rc::from(HTTP_RESPONSE_TAG_VERSION)),
    );
    map.insert("status".to_string(), VmValue::Int(status));
    map.insert(
        "body_kind".to_string(),
        VmValue::String(Rc::from(body_kind)),
    );
    map.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
    if !matches!(body, VmValue::Nil) {
        map.insert("body".to_string(), body);
    }
    map
}

fn require_status(value: Option<&VmValue>, fn_name: &str) -> Result<i64, VmError> {
    let status = match value {
        Some(VmValue::Int(value)) => *value,
        Some(other) => {
            return Err(thrown_err(format!(
                "{fn_name}: status must be an integer (got {})",
                other.type_name()
            )));
        }
        None => {
            return Err(thrown_err(format!("{fn_name}: status is required")));
        }
    };
    if !(100..=599).contains(&status) {
        return Err(thrown_err(format!(
            "{fn_name}: status {status} is out of range (100-599)"
        )));
    }
    Ok(status)
}

fn require_nonempty_string(
    value: Option<&VmValue>,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    let text = match value {
        Some(VmValue::String(text)) => text.to_string(),
        Some(other) => {
            return Err(thrown_err(format!(
                "{fn_name}: {arg_name} must be a string (got {})",
                other.type_name()
            )));
        }
        None => {
            return Err(thrown_err(format!("{fn_name}: {arg_name} is required")));
        }
    };
    if text.is_empty() {
        return Err(thrown_err(format!(
            "{fn_name}: {arg_name} must be non-empty"
        )));
    }
    Ok(text)
}

fn string_or_nil(value: &VmValue) -> Option<String> {
    match value {
        VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn parse_headers(
    value: Option<&VmValue>,
    fn_name: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(dict)) => Ok((**dict).clone()),
        Some(other) => Err(thrown_err(format!(
            "{fn_name}: headers must be a dict (got {})",
            other.type_name()
        ))),
    }
}

/// Drain a channel into a `Vec<VmValue>`, or pass a list through verbatim.
///
/// The drain stops when the channel is closed and all queued values have
/// been consumed. Note: `close_channel(chan)` sets a flag — it does
/// **not** drop the channel's `Sender`, so a naive `rx.recv().await`
/// would block forever for channel-typed sources. We instead poll with
/// `try_recv` and observe the `closed` flag; when both report empty,
/// the drain terminates.
async fn drain_to_list(value: VmValue, fn_name: &str) -> Result<Vec<VmValue>, VmError> {
    use std::sync::atomic::Ordering;
    use tokio::sync::mpsc::error::TryRecvError;

    match value {
        VmValue::List(items) => Ok(items.iter().cloned().collect()),
        VmValue::Channel(handle) => {
            let mut items = Vec::new();
            let mut rx = handle.receiver.lock().await;
            loop {
                match rx.try_recv() {
                    Ok(value) => items.push(value),
                    Err(TryRecvError::Empty) => {
                        if handle.closed.load(Ordering::SeqCst) {
                            break;
                        }
                        // Yield so the producer can deliver the next
                        // value; if the channel is still empty after
                        // the producer has done its work, the next
                        // pass will see it closed.
                        tokio::task::yield_now().await;
                    }
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            Ok(items)
        }
        other => Err(thrown_err(format!(
            "{fn_name}: source must be a list or channel (got {})",
            other.type_name()
        ))),
    }
}

fn thrown_err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

/// Return `Some(envelope)` if the JSON value is a tagged HTTP response.
///
/// Used by the harn-serve HTTP codec to detect that a `.harn` handler
/// opted into structured HTTP semantics rather than the default `200
/// {result}` shape.
pub fn parse_envelope(value: &serde_json::Value) -> Option<HttpEnvelope> {
    let obj = value.as_object()?;
    let tag = obj.get(HTTP_RESPONSE_TAG_KEY)?.as_str()?;
    if tag != HTTP_RESPONSE_TAG_VERSION {
        return None;
    }
    let status = obj.get("status")?.as_u64()? as u16;
    let body_kind = obj
        .get("body_kind")
        .and_then(|v| v.as_str())
        .unwrap_or(BODY_KIND_JSON)
        .to_string();
    let headers = obj
        .get("headers")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    let header = match value {
                        serde_json::Value::String(s) => HttpHeaderValue::Single(s.clone()),
                        serde_json::Value::Array(values) => HttpHeaderValue::Multi(
                            values
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect(),
                        ),
                        other => HttpHeaderValue::Single(other.to_string()),
                    };
                    (key.clone(), header)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let body = obj.get("body").cloned();
    let retry_ms = obj.get("retry_ms").and_then(|v| v.as_u64());
    let is_error = obj
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ws_upgrade = obj
        .get("ws_upgrade")
        .and_then(|v| v.as_object())
        .map(|map| {
            let subprotocol = map
                .get("subprotocol")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let offered = map
                .get("offered")
                .and_then(|v| v.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let idle_ping_ms = map.get("idle_ping_ms").and_then(|v| v.as_u64());
            let max_message_bytes = map.get("max_message_bytes").and_then(|v| v.as_u64());
            WsUpgradeSpec {
                subprotocol,
                offered,
                idle_ping_ms,
                max_message_bytes,
            }
        });
    Some(HttpEnvelope {
        status,
        body_kind,
        headers,
        body,
        retry_ms,
        is_error,
        ws_upgrade,
    })
}

#[derive(Debug, Clone)]
pub struct HttpEnvelope {
    pub status: u16,
    pub body_kind: String,
    pub headers: BTreeMap<String, HttpHeaderValue>,
    pub body: Option<serde_json::Value>,
    pub retry_ms: Option<u64>,
    pub is_error: bool,
    /// Populated when the handler returned an `http_upgrade_ws(...)`
    /// envelope. The hosting adapter detects this and routes the
    /// upgrade through `harn_serve::ws_route` instead of rendering the
    /// 101 response as plain HTTP.
    pub ws_upgrade: Option<WsUpgradeSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct WsUpgradeSpec {
    pub subprotocol: Option<String>,
    pub offered: Vec<String>,
    pub idle_ping_ms: Option<u64>,
    pub max_message_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum HttpHeaderValue {
    Single(String),
    Multi(Vec<String>),
}

impl HttpHeaderValue {
    pub fn values(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        match self {
            Self::Single(value) => Box::new(std::iter::once(value.as_str())),
            Self::Multi(values) => Box::new(values.iter().map(String::as_str)),
        }
    }
}

#[harn_builtin(sig = "http_etag(body: any) -> string", category = "http_response")]
fn http_etag_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let body = args
        .first()
        .ok_or_else(|| thrown_err("http_etag: body is required"))?;
    let bytes = value_as_bytes(body);
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    Ok(VmValue::String(Rc::from(format!(
        "\"{}\"",
        hex::encode(digest)
    ))))
}

#[harn_builtin(
    sig = "http_choose(accept: string?, offers: list, default?: string?) -> string",
    category = "http_response"
)]
fn http_choose_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let accept = optional_string_arg(args.first(), "http_choose", "accept")?;
    let offers_value = args
        .get(1)
        .ok_or_else(|| thrown_err("http_choose: offers is required"))?;
    let offers = expect_string_list(offers_value, "http_choose", "offers")?;
    if offers.is_empty() {
        return Err(thrown_err("http_choose: offers must be non-empty"));
    }
    let default = optional_string_arg(args.get(2), "http_choose", "default")?
        .unwrap_or_else(|| offers[0].clone());

    let chosen = match accept.as_deref() {
        None | Some("") | Some("*/*") => default,
        Some(header) => negotiate_accept(header, &offers).unwrap_or(default),
    };
    Ok(VmValue::String(Rc::from(chosen)))
}

fn optional_string_arg(
    value: Option<&VmValue>,
    builtin: &str,
    arg_name: &str,
) -> Result<Option<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => Ok(Some(text.to_string())),
        Some(other) => Err(thrown_err(format!(
            "{builtin}: {arg_name} must be a string or nil (got {})",
            other.type_name()
        ))),
    }
}

fn expect_string_list(
    value: &VmValue,
    builtin: &str,
    arg_name: &str,
) -> Result<Vec<String>, VmError> {
    let items = match value {
        VmValue::List(items) => items,
        other => {
            return Err(thrown_err(format!(
                "{builtin}: {arg_name} must be a list (got {})",
                other.type_name()
            )));
        }
    };
    items
        .iter()
        .map(|value| match value {
            VmValue::String(text) => Ok(text.to_string()),
            other => Err(thrown_err(format!(
                "{builtin}: {arg_name} must contain strings (got {})",
                other.type_name()
            ))),
        })
        .collect()
}

#[harn_builtin(
    sig = "http_not_modified(etag?: string?, headers?: dict) -> dict",
    category = "http_response"
)]
fn http_not_modified_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut headers = parse_headers(args.get(1), "http_not_modified")?;
    if let Some(etag) = args.first().and_then(string_or_nil) {
        headers.insert("ETag".to_string(), VmValue::String(Rc::from(etag)));
    }
    Ok(envelope(304, VmValue::Nil, BODY_KIND_NONE, headers))
}

#[harn_builtin(
    sig = "http_upgrade_ws(req: dict, options?: dict) -> dict",
    category = "http_response"
)]
fn http_upgrade_ws_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let req = args
        .first()
        .and_then(VmValue::as_dict)
        .ok_or_else(|| thrown_err("http_upgrade_ws: req must be a dict"))?;
    let options = args.get(1).and_then(VmValue::as_dict);

    let request_subprotocols = req
        .get("headers")
        .and_then(VmValue::as_dict)
        .and_then(|headers| header_lookup(headers, "sec-websocket-protocol"))
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let offered_subprotocols = options
        .and_then(|opts| opts.get("subprotocols"))
        .and_then(|value| match value {
            VmValue::List(items) => Some(
                items
                    .iter()
                    .filter_map(|v| match v {
                        VmValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    let negotiated = offered_subprotocols
        .iter()
        .find(|name| request_subprotocols.iter().any(|client| client == *name))
        .cloned();

    let mut headers = BTreeMap::new();
    headers.insert(
        "Upgrade".to_string(),
        VmValue::String(Rc::from("websocket")),
    );
    headers.insert(
        "Connection".to_string(),
        VmValue::String(Rc::from("Upgrade")),
    );
    if let Some(name) = &negotiated {
        headers.insert(
            "Sec-WebSocket-Protocol".to_string(),
            VmValue::String(Rc::from(name.clone())),
        );
    }

    let idle_ping_ms = options
        .and_then(|opts| opts.get("idle_ping_ms"))
        .and_then(|v| v.as_int());
    let max_message_bytes = options
        .and_then(|opts| opts.get("max_message_bytes"))
        .and_then(|v| v.as_int());

    let mut env_map = envelope_map(101, VmValue::Nil, BODY_KIND_NONE, headers);
    env_map.insert(
        "ws_upgrade".to_string(),
        VmValue::Dict(Rc::new({
            let mut map = BTreeMap::new();
            map.insert(
                "subprotocol".to_string(),
                match &negotiated {
                    Some(name) => VmValue::String(Rc::from(name.clone())),
                    None => VmValue::Nil,
                },
            );
            map.insert(
                "offered".to_string(),
                VmValue::List(Rc::new(
                    offered_subprotocols
                        .iter()
                        .map(|s| VmValue::String(Rc::from(s.clone())))
                        .collect(),
                )),
            );
            if let Some(ms) = idle_ping_ms {
                map.insert("idle_ping_ms".to_string(), VmValue::Int(ms));
            }
            if let Some(bytes) = max_message_bytes {
                map.insert("max_message_bytes".to_string(), VmValue::Int(bytes));
            }
            map
        })),
    );
    Ok(VmValue::Dict(Rc::new(env_map)))
}

fn header_lookup(headers: &BTreeMap<String, VmValue>, name: &str) -> Option<String> {
    let needle = name.to_ascii_lowercase();
    headers
        .iter()
        .find(|(key, _)| key.to_ascii_lowercase() == needle)
        .and_then(|(_, value)| match value {
            VmValue::String(text) => Some(text.to_string()),
            _ => None,
        })
}

fn value_as_bytes(value: &VmValue) -> Vec<u8> {
    match value {
        VmValue::Bytes(bytes) => bytes.as_ref().clone(),
        VmValue::String(text) => text.as_bytes().to_vec(),
        VmValue::Nil => Vec::new(),
        // For dicts / lists / structs, fall through to the stdlib JSON
        // encoder so the ETag derives from a stable canonical form
        // (dict keys sorted, bytes base64-tagged) rather than the
        // less-stable `display()` representation. We reuse
        // `stdlib::json` directly instead of reaching for the
        // llm-helpers encoder so the abstraction boundary stays
        // sibling-module, not cross-subsystem.
        other => crate::stdlib::json::vm_value_to_json(other).into_bytes(),
    }
}

/// Parse an HTTP `Accept` header and return the best matching offer.
///
/// Standard Q-value scoring per RFC 9110 §12.5.1: each media-range
/// gets a `q` (1.0 by default); each offer is scored by its
/// best-matching range, with ties broken by offer order. Wildcard
/// matches (`type/*`, `*/*`) score below exact-type matches.
fn negotiate_accept(header: &str, offers: &[String]) -> Option<String> {
    let ranges: Vec<MediaRange> = header
        .split(',')
        .filter_map(MediaRange::parse)
        .filter(|range| range.q > 0.0)
        .collect();
    if ranges.is_empty() {
        return None;
    }

    let mut best: Option<(usize, f32, u8)> = None;
    for (index, offer) in offers.iter().enumerate() {
        let (offer_type, offer_subtype) = split_media(offer)?;
        for range in &ranges {
            let score = range.match_score(offer_type, offer_subtype);
            let Some(score) = score else { continue };
            let q = range.q;
            let candidate = (index, q, score);
            best = Some(match best {
                None => candidate,
                Some(current) => {
                    // Prefer higher q first; if equal, higher specificity;
                    // if equal, earlier offer wins.
                    if q > current.1
                        || (q == current.1 && score > current.2)
                        || (q == current.1 && score == current.2 && index < current.0)
                    {
                        candidate
                    } else {
                        current
                    }
                }
            });
        }
    }
    best.map(|(index, _, _)| offers[index].clone())
}

struct MediaRange<'a> {
    type_: &'a str,
    subtype: &'a str,
    q: f32,
}

impl<'a> MediaRange<'a> {
    fn parse(raw: &'a str) -> Option<Self> {
        let trimmed = raw.trim();
        let mut parts = trimmed.split(';');
        let media = parts.next()?.trim();
        let (type_, subtype) = split_media(media)?;
        let mut q = 1.0;
        for param in parts {
            let param = param.trim();
            if let Some(value) = param
                .strip_prefix("q=")
                .or_else(|| param.strip_prefix("Q="))
            {
                if let Ok(parsed) = value.trim().parse::<f32>() {
                    if (0.0..=1.0).contains(&parsed) {
                        q = parsed;
                    }
                }
            }
        }
        Some(Self { type_, subtype, q })
    }

    fn match_score(&self, offer_type: &str, offer_subtype: &str) -> Option<u8> {
        let type_match = self.type_ == "*" || self.type_.eq_ignore_ascii_case(offer_type);
        let subtype_match = self.subtype == "*" || self.subtype.eq_ignore_ascii_case(offer_subtype);
        if !type_match || !subtype_match {
            return None;
        }
        Some(match (self.type_, self.subtype) {
            ("*", _) => 1,
            (_, "*") => 2,
            _ => 3,
        })
    }
}

fn split_media(value: &str) -> Option<(&str, &str)> {
    let mut iter = value.splitn(2, '/');
    let type_ = iter.next()?.trim();
    let subtype = iter.next()?.trim();
    if type_.is_empty() || subtype.is_empty() {
        return None;
    }
    Some((type_, subtype))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &HTTP_OK_IMPL_DEF,
    &HTTP_CREATED_IMPL_DEF,
    &HTTP_NO_CONTENT_IMPL_DEF,
    &HTTP_ERROR_IMPL_DEF,
    &HTTP_REPLY_IMPL_DEF,
    &HTTP_STREAM_IMPL_DEF,
    &HTTP_SSE_IMPL_DEF,
    &HTTP_ETAG_IMPL_DEF,
    &HTTP_CHOOSE_IMPL_DEF,
    &HTTP_NOT_MODIFIED_IMPL_DEF,
    &HTTP_UPGRADE_WS_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::helpers::vm_value_to_json;

    fn dict(value: &VmValue) -> &BTreeMap<String, VmValue> {
        value.as_dict().expect("envelope is a dict")
    }

    fn run_sync<F, Fut>(future: F) -> Fut::Output
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future,
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(future())
    }

    #[test]
    fn http_ok_produces_tagged_envelope() {
        let body = VmValue::String(Rc::from("hello"));
        let response = http_ok_impl(&[body], &mut String::new()).expect("ok");
        let map = dict(&response);
        assert_eq!(
            map.get(HTTP_RESPONSE_TAG_KEY).and_then(|v| match v {
                VmValue::String(s) => Some(s.as_ref()),
                _ => None,
            }),
            Some(HTTP_RESPONSE_TAG_VERSION)
        );
        assert!(matches!(map.get("status"), Some(VmValue::Int(200))));
        assert_eq!(
            map.get("body").map(|v| v.display()).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn http_created_sets_location_header() {
        let body = VmValue::Dict(Rc::new(BTreeMap::from([(
            "id".to_string(),
            VmValue::String(Rc::from("sess_1")),
        )])));
        let location = VmValue::String(Rc::from("/v1/sessions/sess_1"));
        let response = http_created_impl(&[body, location], &mut String::new()).expect("created");
        let map = dict(&response);
        assert!(matches!(map.get("status"), Some(VmValue::Int(201))));
        let headers = map
            .get("headers")
            .and_then(VmValue::as_dict)
            .expect("headers");
        assert_eq!(
            headers.get("Location").map(|v| v.display()).as_deref(),
            Some("/v1/sessions/sess_1")
        );
    }

    #[test]
    fn http_no_content_omits_body_marker() {
        let response = http_no_content_impl(&[], &mut String::new()).expect("no_content");
        let map = dict(&response);
        assert!(matches!(map.get("status"), Some(VmValue::Int(204))));
        assert!(map.get("body").is_none());
        assert_eq!(
            map.get("body_kind").and_then(|v| match v {
                VmValue::String(s) => Some(s.as_ref()),
                _ => None,
            }),
            Some(BODY_KIND_NONE)
        );
    }

    #[test]
    fn http_error_carries_code_message_and_marker() {
        let response = http_error_impl(
            &[
                VmValue::Int(422),
                VmValue::String(Rc::from("invalid_input")),
                VmValue::String(Rc::from("bad payload")),
                VmValue::Nil,
            ],
            &mut String::new(),
        )
        .expect("error");
        let map = dict(&response);
        assert!(matches!(map.get("status"), Some(VmValue::Int(422))));
        assert!(matches!(map.get("is_error"), Some(VmValue::Bool(true))));
        let body = map
            .get("body")
            .and_then(VmValue::as_dict)
            .expect("body dict");
        assert_eq!(
            body.get("code").map(|v| v.display()).as_deref(),
            Some("invalid_input")
        );
        assert_eq!(
            body.get("message").map(|v| v.display()).as_deref(),
            Some("bad payload")
        );
    }

    #[test]
    fn http_error_rejects_2xx_status() {
        let err = http_error_impl(
            &[
                VmValue::Int(200),
                VmValue::String(Rc::from("x")),
                VmValue::String(Rc::from("y")),
            ],
            &mut String::new(),
        )
        .expect_err("expected reject");
        match err {
            VmError::Thrown(VmValue::String(text)) => {
                assert!(text.contains("4xx or 5xx"), "got: {text}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn http_reply_rejects_out_of_range_status() {
        let err =
            http_reply_impl(&[VmValue::Int(999)], &mut String::new()).expect_err("out of range");
        match err {
            VmError::Thrown(VmValue::String(text)) => {
                assert!(text.contains("100-599"), "got: {text}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn http_stream_buffers_list_source() {
        let items = vec![
            VmValue::String(Rc::from("a")),
            VmValue::String(Rc::from("b")),
        ];
        let response = run_sync(|| {
            http_stream_impl(vec![
                VmValue::List(Rc::new(items.clone())),
                VmValue::String(Rc::from("text/plain")),
            ])
        })
        .expect("stream");
        let map = dict(&response);
        assert_eq!(
            map.get("body_kind").and_then(|v| match v {
                VmValue::String(s) => Some(s.as_ref()),
                _ => None,
            }),
            Some(BODY_KIND_STREAM)
        );
        let body = map.get("body").expect("body");
        match body {
            VmValue::List(values) => {
                assert_eq!(values.len(), 2);
            }
            other => panic!("expected list body, got {other:?}"),
        }
        let headers = map
            .get("headers")
            .and_then(VmValue::as_dict)
            .expect("headers");
        assert_eq!(
            headers.get("Content-Type").map(|v| v.display()).as_deref(),
            Some("text/plain")
        );
    }

    #[test]
    fn http_sse_sets_event_stream_headers_and_optional_retry() {
        let events = vec![VmValue::Dict(Rc::new(BTreeMap::from([(
            "data".to_string(),
            VmValue::String(Rc::from("ping")),
        )])))];
        let response = run_sync(|| {
            http_sse_impl(vec![
                VmValue::List(Rc::new(events.clone())),
                VmValue::Int(2500),
            ])
        })
        .expect("sse");
        let map = dict(&response);
        let headers = map
            .get("headers")
            .and_then(VmValue::as_dict)
            .expect("headers");
        assert_eq!(
            headers.get("Content-Type").map(|v| v.display()).as_deref(),
            Some("text/event-stream")
        );
        assert_eq!(
            headers.get("Cache-Control").map(|v| v.display()).as_deref(),
            Some("no-cache")
        );
        assert!(matches!(map.get("retry_ms"), Some(VmValue::Int(2500))));
    }

    #[test]
    fn parse_envelope_round_trip_through_json() {
        let response = http_error_impl(
            &[
                VmValue::Int(404),
                VmValue::String(Rc::from("not_found")),
                VmValue::String(Rc::from("missing")),
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "id".to_string(),
                    VmValue::String(Rc::from("sess_404")),
                )]))),
            ],
            &mut String::new(),
        )
        .expect("error");
        let json = vm_value_to_json(&response);
        let envelope = parse_envelope(&json).expect("envelope parses");
        assert_eq!(envelope.status, 404);
        assert!(envelope.is_error);
        let body = envelope.body.expect("body");
        assert_eq!(body["code"], "not_found");
        assert_eq!(body["details"]["id"], "sess_404");
    }

    #[test]
    fn parse_envelope_ignores_untagged_dicts() {
        let plain = serde_json::json!({"status": 200, "body": {}});
        assert!(parse_envelope(&plain).is_none());
    }

    #[test]
    fn http_etag_is_quoted_hex_sha256_of_payload() {
        let value = VmValue::String(Rc::from("hello"));
        let etag = http_etag_impl(&[value], &mut String::new()).expect("etag");
        match etag {
            VmValue::String(text) => {
                assert_eq!(
                    text.as_ref(),
                    "\"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\""
                );
            }
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn http_etag_stable_across_string_and_bytes_for_same_payload() {
        let from_string =
            http_etag_impl(&[VmValue::String(Rc::from("hello"))], &mut String::new()).unwrap();
        let from_bytes = http_etag_impl(
            &[VmValue::Bytes(Rc::new(b"hello".to_vec()))],
            &mut String::new(),
        )
        .unwrap();
        assert_eq!(from_string.display(), from_bytes.display());
    }

    #[test]
    fn http_choose_returns_best_q_match() {
        let accept = VmValue::String(Rc::from("application/xml;q=0.5, application/json;q=0.9"));
        let offers = VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("application/xml")),
            VmValue::String(Rc::from("application/json")),
        ]));
        let chosen = http_choose_impl(&[accept, offers], &mut String::new()).unwrap();
        assert_eq!(chosen.display(), "application/json");
    }

    #[test]
    fn http_choose_prefers_specific_over_wildcard() {
        let accept = VmValue::String(Rc::from("text/*;q=0.5, application/json"));
        let offers = VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("text/plain")),
            VmValue::String(Rc::from("application/json")),
        ]));
        let chosen = http_choose_impl(&[accept, offers], &mut String::new()).unwrap();
        assert_eq!(chosen.display(), "application/json");
    }

    #[test]
    fn http_choose_returns_default_for_no_accept() {
        let offers = VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("text/plain")),
            VmValue::String(Rc::from("application/json")),
        ]));
        let chosen = http_choose_impl(&[VmValue::Nil, offers], &mut String::new()).unwrap();
        assert_eq!(chosen.display(), "text/plain");
    }

    #[test]
    fn http_choose_overrides_default_with_explicit() {
        let offers = VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("text/plain")),
            VmValue::String(Rc::from("application/json")),
        ]));
        let chosen = http_choose_impl(
            &[
                VmValue::Nil,
                offers,
                VmValue::String(Rc::from("application/json")),
            ],
            &mut String::new(),
        )
        .unwrap();
        assert_eq!(chosen.display(), "application/json");
    }

    #[test]
    fn http_choose_wildcard_accept_yields_default() {
        let offers = VmValue::List(Rc::new(vec![VmValue::String(Rc::from("application/json"))]));
        let chosen = http_choose_impl(
            &[VmValue::String(Rc::from("*/*")), offers],
            &mut String::new(),
        )
        .unwrap();
        assert_eq!(chosen.display(), "application/json");
    }

    #[test]
    fn http_not_modified_envelope_carries_etag() {
        let etag = VmValue::String(Rc::from("\"abc\""));
        let response = http_not_modified_impl(&[etag, VmValue::Nil], &mut String::new()).unwrap();
        let map = dict(&response);
        assert!(matches!(map.get("status"), Some(VmValue::Int(304))));
        let headers = map
            .get("headers")
            .and_then(VmValue::as_dict)
            .expect("headers");
        assert_eq!(
            headers.get("ETag").map(|v| v.display()).as_deref(),
            Some("\"abc\"")
        );
    }

    #[test]
    fn http_upgrade_ws_envelope_negotiates_subprotocol() {
        let req = VmValue::Dict(Rc::new(BTreeMap::from([(
            "headers".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "Sec-WebSocket-Protocol".to_string(),
                VmValue::String(Rc::from("v0.harn, v1.harn")),
            )]))),
        )])));
        let options = VmValue::Dict(Rc::new(BTreeMap::from([(
            "subprotocols".to_string(),
            VmValue::List(Rc::new(vec![
                VmValue::String(Rc::from("v1.harn")),
                VmValue::String(Rc::from("v2.harn")),
            ])),
        )])));
        let response = http_upgrade_ws_impl(&[req, options], &mut String::new()).unwrap();
        let map = dict(&response);
        assert!(matches!(map.get("status"), Some(VmValue::Int(101))));
        let upgrade = map
            .get("ws_upgrade")
            .and_then(VmValue::as_dict)
            .expect("ws_upgrade");
        assert_eq!(
            upgrade.get("subprotocol").map(|v| v.display()).as_deref(),
            Some("v1.harn")
        );
        let headers = map
            .get("headers")
            .and_then(VmValue::as_dict)
            .expect("headers");
        assert_eq!(
            headers.get("Upgrade").map(|v| v.display()).as_deref(),
            Some("websocket")
        );
        assert_eq!(
            headers
                .get("Sec-WebSocket-Protocol")
                .map(|v| v.display())
                .as_deref(),
            Some("v1.harn")
        );
    }

    #[test]
    fn parse_envelope_round_trips_ws_upgrade_marker() {
        let req = VmValue::Dict(Rc::new(BTreeMap::from([(
            "headers".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "Sec-WebSocket-Protocol".to_string(),
                VmValue::String(Rc::from("v1.harn")),
            )]))),
        )])));
        let options = VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "subprotocols".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("v1.harn"))])),
            ),
            ("idle_ping_ms".to_string(), VmValue::Int(15_000)),
        ])));
        let response = http_upgrade_ws_impl(&[req, options], &mut String::new()).unwrap();
        let json = vm_value_to_json(&response);
        let envelope = parse_envelope(&json).expect("envelope parses");
        let ws = envelope.ws_upgrade.expect("ws_upgrade present");
        assert_eq!(ws.subprotocol.as_deref(), Some("v1.harn"));
        assert_eq!(ws.offered, vec!["v1.harn"]);
        assert_eq!(ws.idle_ping_ms, Some(15_000));
        assert_eq!(envelope.status, 101);
    }

    #[test]
    fn http_upgrade_ws_falls_through_when_no_subprotocols_offered() {
        let req = VmValue::Dict(Rc::new(BTreeMap::new()));
        let response = http_upgrade_ws_impl(&[req], &mut String::new()).unwrap();
        let map = dict(&response);
        let upgrade = map
            .get("ws_upgrade")
            .and_then(VmValue::as_dict)
            .expect("ws_upgrade");
        assert!(matches!(upgrade.get("subprotocol"), Some(VmValue::Nil)));
    }
}
