//! Fake stable MCP clients used to validate Harn's MCP server behavior.
//!
//! Two flavors: a JSON-only "wire builder" that constructs stable-tagged
//! request payloads with `_meta` blocks, and an HTTP driver that posts
//! those payloads at a running Harn MCP HTTP server and asserts on the
//! response. The wire builder is reused by stdio-flavored tests that
//! pipe the same payloads into a process or in-process service.

use harn_vm::mcp_protocol::{
    self, MCP_HEADER_METHOD, MCP_HEADER_NAME, MCP_HEADER_PROTOCOL_VERSION,
    MCP_META_KEY_CLIENT_CAPABILITIES, MCP_META_KEY_CLIENT_INFO, MCP_META_KEY_PROTOCOL_VERSION,
    PROTOCOL_VERSION,
};
use serde_json::{json, Value as JsonValue};

/// Build a stable `_meta` block targeting the released protocol version. The
/// trio of keys is the minimum every stable client must send so servers can
/// negotiate per-request without sticky state.
pub fn stable_meta(client_name: &str) -> JsonValue {
    json!({
        MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
        MCP_META_KEY_CLIENT_INFO: {"name": client_name, "version": "0.1.0"},
        MCP_META_KEY_CLIENT_CAPABILITIES: {},
    })
}

/// Build a request body for the given method with stable metadata folded
/// in. Pass `params` for additional fields; `_meta` is merged in.
pub fn stable_request(id: u64, method: &str, params: JsonValue, client_name: &str) -> JsonValue {
    let mut params = params.as_object().cloned().unwrap_or_default();
    let mut meta = params
        .remove("_meta")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let stable = stable_meta(client_name)
        .as_object()
        .expect("stable metadata is an object")
        .clone();
    // Preserve request-scoped values such as progressToken while making the
    // stable protocol identity authoritative.
    meta.extend(stable);
    params.insert("_meta".to_string(), JsonValue::Object(meta));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": JsonValue::Object(params),
    })
}

/// Compose the stable HTTP header set a Stable client sends with a
/// JSON-RPC body. Includes `MCP-Protocol-Version`, `Mcp-Method`, and
/// the optional `Mcp-Name` derived from the body via the spec helper.
pub fn stable_headers(method: &str, params: &JsonValue) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        (MCP_HEADER_PROTOCOL_VERSION, PROTOCOL_VERSION.to_string()),
        (MCP_HEADER_METHOD, method.to_string()),
    ];
    if let Some(name) = mcp_protocol::standard_name_header_value(method, params) {
        headers.push((MCP_HEADER_NAME, name));
    }
    headers
}

/// HTTP driver: POST `body` to `url` with `headers`, return the parsed
/// JSON response and the protocol header the server echoed back.
///
/// Servers are allowed to answer JSON-RPC POSTs with either
/// `application/json` or an SSE stream that carries the response as a
/// `message` event (per MCP Streamable HTTP). The driver advertises
/// both, then dispatches on the response `Content-Type` so the test
/// surface looks identical to the caller regardless of which the
/// server chose.
pub async fn post_mcp(
    url: &str,
    body: &JsonValue,
    headers: &[(&'static str, String)],
) -> McpResponse {
    let client = reqwest::Client::new();
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = request.json(body).send().await.expect("post MCP request");
    let status = response.status().as_u16();
    let echoed_protocol = response
        .headers()
        .get(MCP_HEADER_PROTOCOL_VERSION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let echoed_session = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    let body = if content_type.contains("text/event-stream") {
        let text = response.text().await.expect("read sse body");
        parse_sse_message_body(&text)
    } else {
        response.json::<JsonValue>().await.expect("parse json body")
    };
    McpResponse {
        status,
        echoed_protocol,
        echoed_session,
        body,
    }
}

/// Extract the JSON body of the first `message` event from an SSE
/// stream. Tests only need the response payload, so we ignore everything
/// else — keep-alives, comments, the priming empty event, etc.
fn parse_sse_message_body(text: &str) -> JsonValue {
    let mut data = String::new();
    let mut current_event: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            if current_event.as_deref() == Some("message") && !data.is_empty() {
                if let Ok(value) = serde_json::from_str(&data) {
                    return value;
                }
            }
            current_event = None;
            data.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            current_event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    // Trailing event without blank-line terminator.
    if current_event.as_deref() == Some("message") && !data.is_empty() {
        if let Ok(value) = serde_json::from_str(&data) {
            return value;
        }
    }
    panic!("SSE stream contained no parseable message event; raw: {text:?}");
}

/// Inspect a JSON-RPC result and assert the stable envelope fields are
/// present. Returns the original result body for chained assertions.
pub fn assert_stable_envelope(result: &JsonValue) -> &JsonValue {
    let result_obj = result.get("result").expect("result object");
    assert_eq!(
        result_obj.get("resultType").and_then(JsonValue::as_str),
        Some("complete"),
        "stable result must carry resultType=complete"
    );
    result_obj
}

#[derive(Debug)]
pub struct McpResponse {
    pub status: u16,
    pub echoed_protocol: Option<String>,
    pub echoed_session: Option<String>,
    pub body: JsonValue,
}
