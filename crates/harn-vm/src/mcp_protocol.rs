//! Harn policy layered on the official MCP Rust SDK protocol model.
//!
//! `rmcp` owns protocol versions, standard error codes, typed messages, and
//! lifecycle semantics. This module retains only Harn policy that is not part
//! of MCP itself: pagination limits, cache defaults, and conversion helpers for
//! the VM's JSON-valued interface.

use serde_json::{json, Value as JsonValue};

mod header_value;
pub use header_value::{
    decode_header_value, encode_header_value, header_value_examples, McpHeaderValueEncoding,
    MCP_HEADER_VALUE_ENCODING,
};

/// Stable MCP version used by Harn's discover/per-request lifecycle.
///
/// Keep this literal mechanically checked against `rmcp` below because public
/// Harn projections require a `&'static str` while the SDK exposes a typed
/// [`rmcp::model::ProtocolVersion`].
pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const METHOD_SERVER_DISCOVER: &str = "server/discover";
pub const METHOD_TOOLS_LIST: &str =
    <rmcp::model::ListToolsRequestMethod as rmcp::model::ConstString>::VALUE;
pub const METHOD_TOOLS_CALL: &str =
    <rmcp::model::CallToolRequestMethod as rmcp::model::ConstString>::VALUE;
pub const METHOD_CANCELLED_NOTIFICATION: &str =
    <rmcp::model::CancelledNotificationMethod as rmcp::model::ConstString>::VALUE;
pub const METHOD_TASKS_GET: &str = "tasks/get";
pub const METHOD_TASKS_UPDATE: &str = "tasks/update";
pub const METHOD_TASKS_CANCEL: &str = "tasks/cancel";
pub const METHOD_COMPLETION_COMPLETE: &str = "completion/complete";
pub const METHOD_SAMPLING_CREATE_MESSAGE: &str = "sampling/createMessage";
pub const METHOD_ELICITATION_CREATE: &str = "elicitation/create";
pub const TASKS_EXTENSION_ID: &str = rmcp::model::TASKS_EXTENSION_ID;
pub const METHOD_ROOTS_LIST: &str = "roots/list";
pub const METHOD_ROOTS_LIST_CHANGED_NOTIFICATION: &str = "notifications/roots/list_changed";

/// Stable per-request metadata keys carried inside `params._meta`.
pub const MCP_META_KEY_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const MCP_META_KEY_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub const MCP_META_KEY_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub const MCP_META_KEY_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Request fields projected into the stable HTTP transport's `Mcp-Name` header.
/// Both Harn requests and generated SDKs consume this same protocol table.
pub const MCP_NAME_FIELDS: &[(&str, &str)] = &[
    (METHOD_TOOLS_CALL, "name"),
    ("prompts/get", "name"),
    ("resources/read", "uri"),
    (METHOD_TASKS_GET, "taskId"),
    (METHOD_TASKS_UPDATE, "taskId"),
    (METHOD_TASKS_CANCEL, "taskId"),
];

/// Validate one application-owned MCP `_meta` key.
///
/// Harn authors protocol keys itself. Catalog extensions must therefore use
/// the standard key grammar without claiming an MCP-reserved vendor prefix.
pub fn validate_application_meta_key(key: &str) -> Result<(), String> {
    let (prefix, name) = match key.split_once('/') {
        Some((prefix, name)) if !name.contains('/') => (Some(prefix), name),
        Some(_) => return Err("must contain at most one '/' separator".to_string()),
        None => (None, key),
    };

    if let Some(prefix) = prefix {
        let labels: Vec<_> = prefix.split('.').collect();
        if labels.iter().any(|label| {
            let bytes = label.as_bytes();
            bytes.is_empty()
                || !bytes[0].is_ascii_alphabetic()
                || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
                || !bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        }) {
            return Err("has an invalid reverse-DNS prefix".to_string());
        }
        if labels
            .get(1)
            .is_some_and(|label| *label == "mcp" || *label == "modelcontextprotocol")
        {
            return Err("uses an MCP-reserved prefix".to_string());
        }
    }

    if name.is_empty() {
        return Err("has an empty metadata name".to_string());
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.'))
    {
        return Err("has an invalid metadata name".to_string());
    }
    Ok(())
}

/// Standard HTTP routing headers for the stable protocol.
pub const MCP_HEADER_PROTOCOL_VERSION: &str =
    rmcp::transport::common::http_header::HEADER_MCP_PROTOCOL_VERSION;
pub const MCP_HEADER_METHOD: &str = rmcp::transport::common::http_header::HEADER_MCP_METHOD;
pub const MCP_HEADER_NAME: &str = rmcp::transport::common::http_header::HEADER_MCP_NAME;

/// `resultType` discriminants exposed to 2026 clients on every response.
pub const RESULT_TYPE_COMPLETE: &str = "complete";
pub const RESULT_TYPE_INPUT_REQUIRED: &str = "input_required";

/// Stable MCP protocol errors, projected from the SDK's typed registry.
pub const UNSUPPORTED_PROTOCOL_VERSION_CODE: i64 =
    rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0 as i64;
pub const MISSING_REQUIRED_CLIENT_CAPABILITY_CODE: i64 =
    rmcp::model::ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY.0 as i64;
pub const HEADER_MISMATCH_CODE: i64 = rmcp::model::ErrorCode::HEADER_MISMATCH.0 as i64;

pub const DEFAULT_TASK_POLL_INTERVAL_MS: u64 = 250;
pub const DEFAULT_MCP_LIST_PAGE_SIZE: usize = 100;
pub const MCP_LIST_PAGE_SIZE_ENV: &str = "HARN_MCP_LIST_PAGE_SIZE";

/// Conservative cache hints emitted with list/read results when a stable
/// client asked for them. Both surfaces fall back to these defaults so
/// implementations can opt out per handler if a tighter or looser bound
/// is appropriate.
pub const DEFAULT_LIST_CACHE_TTL_MS: u64 = 5_000;
pub const DEFAULT_LIST_CACHE_SCOPE: &str = "private";
pub const DEFAULT_READ_CACHE_TTL_MS: u64 = 1_000;
pub const DEFAULT_READ_CACHE_SCOPE: &str = "private";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpListPage {
    pub start: usize,
    pub end: usize,
    pub next_cursor: Option<String>,
}

pub const MCP_COMPLETION_MAX_VALUES: usize = 100;

pub use rmcp::model::TaskStatus as McpTaskStatus;

/// Return the SDK task status's canonical wire spelling.
pub fn mcp_task_status_wire_name(status: McpTaskStatus) -> String {
    serde_json::to_value(status)
        .expect("SDK task statuses serialize")
        .as_str()
        .expect("SDK task statuses serialize as strings")
        .to_string()
}

/// Returns every protocol version known to the official SDK.
pub fn sdk_protocol_versions() -> Vec<&'static str> {
    rmcp::model::ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .map(rmcp::model::ProtocolVersion::as_str)
        .collect()
}

pub fn is_sdk_protocol_version(version: &str) -> bool {
    rmcp::model::ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .any(|supported| supported.as_str() == version)
}

/// Returns the SDK-known versions that use metadata on each request.
pub fn request_metadata_protocol_versions() -> Vec<&'static str> {
    rmcp::model::ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .filter(|version| *version >= &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
        .map(rmcp::model::ProtocolVersion::as_str)
        .collect()
}

pub fn is_request_metadata_protocol_version(version: &str) -> bool {
    rmcp::model::ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .filter(|supported| *supported >= &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
        .any(|supported| supported.as_str() == version)
}

fn is_initialize_protocol_version(version: &rmcp::model::ProtocolVersion) -> bool {
    version < &rmcp::model::ProtocolVersion::STANDARD_HEADERS
        && rmcp::model::ProtocolVersion::KNOWN_VERSIONS.contains(version)
}

/// Result of the SDK-typed `initialize` negotiation.
#[derive(Clone, Debug, PartialEq)]
struct McpInitializeOutcome {
    client_identity: String,
    protocol_version: rmcp::model::ProtocolVersion,
    result: JsonValue,
}

/// Negotiate the released MCP initialize lifecycle with official SDK types.
///
/// MCP 2026-07-28 uses `server/discover` and per-request metadata, but released
/// clients still open stdio servers with `initialize`. The SDK supports both
/// lifecycles; this helper keeps Harn's custom dispatch cores aligned without
/// recreating a second protocol model or version registry.
fn negotiate_initialize(
    params: &JsonValue,
    capabilities: JsonValue,
    server_info: JsonValue,
    instructions: Option<&str>,
) -> Result<McpInitializeOutcome, String> {
    let request: rmcp::model::InitializeRequestParams = serde_json::from_value(params.clone())
        .map_err(|error| format!("invalid MCP initialize params: {error}"))?;
    let protocol_version = if is_initialize_protocol_version(&request.protocol_version) {
        request.protocol_version.clone()
    } else {
        rmcp::model::ProtocolVersion::LATEST
    };
    let capabilities: rmcp::model::ServerCapabilities = serde_json::from_value(capabilities)
        .map_err(|error| format!("invalid MCP server capabilities: {error}"))?;
    let server_info: rmcp::model::Implementation = serde_json::from_value(server_info)
        .map_err(|error| format!("invalid MCP server info: {error}"))?;
    let mut result = rmcp::model::InitializeResult::new(capabilities)
        .with_protocol_version(protocol_version.clone())
        .with_server_info(server_info);
    if let Some(instructions) = instructions {
        result = result.with_instructions(instructions);
    }
    let result = serde_json::to_value(result)
        .map_err(|error| format!("failed to encode MCP initialize result: {error}"))?;
    Ok(McpInitializeOutcome {
        client_identity: format!(
            "{}/{}",
            request.client_info.name, request.client_info.version
        ),
        protocol_version,
        result,
    })
}

/// The protocol profile for one accepted server request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpRequestProfile {
    protocol_version: rmcp::model::ProtocolVersion,
}

impl McpRequestProfile {
    /// Stable Streamable HTTP makes closing a direct response an explicit
    /// cancellation. Older resumable profiles do not assign that meaning.
    pub fn cancels_direct_call_on_disconnect(&self) -> bool {
        self.protocol_version >= rmcp::model::ProtocolVersion::V_2026_07_28
    }

    pub fn uses_result_envelope(&self) -> bool {
        self.protocol_version >= rmcp::model::ProtocolVersion::V_2026_07_28
    }
}

/// Connection state shared by Harn's generic and orchestrator MCP servers.
///
/// The state stores only facts negotiated for the connection. Version support,
/// request metadata, and initialize payloads remain owned by official SDK
/// types.
#[derive(Clone, Debug)]
pub struct McpServerSession {
    client_identity: String,
    initialized_protocol_version: Option<rmcp::model::ProtocolVersion>,
    notifications_ready: bool,
}

impl Default for McpServerSession {
    fn default() -> Self {
        Self {
            client_identity: "unknown".to_string(),
            initialized_protocol_version: None,
            notifications_ready: false,
        }
    }
}

impl McpServerSession {
    pub fn client_identity(&self) -> &str {
        &self.client_identity
    }

    pub fn is_ready_for_notifications(&self) -> bool {
        self.notifications_ready
    }

    pub fn accept_initialized_notification(&mut self) {
        if self.initialized_protocol_version.is_some() {
            self.notifications_ready = true;
        }
    }

    pub fn initialize(
        &mut self,
        params: &JsonValue,
        capabilities: JsonValue,
        server_info: JsonValue,
        instructions: Option<&str>,
    ) -> Result<JsonValue, String> {
        let outcome = negotiate_initialize(params, capabilities, server_info, instructions)?;
        self.client_identity = outcome.client_identity;
        self.initialized_protocol_version = Some(outcome.protocol_version);
        self.notifications_ready = false;
        Ok(outcome.result)
    }

    /// Validate one request and return the response profile it negotiated.
    pub fn accept_request(
        &mut self,
        id: &JsonValue,
        method: &str,
        params: &JsonValue,
    ) -> Result<McpRequestProfile, JsonValue> {
        let uses_inline_lifecycle = method == METHOD_SERVER_DISCOVER
            || (self.initialized_protocol_version.is_none() && params.get("_meta").is_some());
        if uses_inline_lifecycle {
            let metadata = parse_request_metadata(params);
            enforce_request_protocol_version(id, &metadata)?;
            self.client_identity = metadata
                .client_info()
                .map(|info| format!("{}/{}", info.name, info.version))
                .unwrap_or_else(|| "unknown".to_string());
            self.notifications_ready = true;
            return Ok(McpRequestProfile {
                protocol_version: rmcp::model::ProtocolVersion::V_2026_07_28,
            });
        }

        if let Some(protocol_version) = &self.initialized_protocol_version {
            return Ok(McpRequestProfile {
                protocol_version: protocol_version.clone(),
            });
        }

        if method == "ping" {
            return Ok(McpRequestProfile {
                protocol_version: rmcp::model::ProtocolVersion::LATEST,
            });
        }

        Err(crate::jsonrpc::error_response(
            id.clone(),
            -32002,
            "server not initialized",
        ))
    }
}

/// The official SDK's typed stable per-request metadata map.
pub use rmcp::model::RequestMetaObject as McpRequestMetadata;

/// Extract stable metadata from a request's `params._meta` block.
pub fn parse_request_metadata(params: &JsonValue) -> McpRequestMetadata {
    let Some(meta) = params.get("_meta") else {
        return McpRequestMetadata::default();
    };
    serde_json::from_value(meta.clone()).unwrap_or_default()
}

/// Validate that a request's metadata targets a supported version.
/// Returns an `Err(error_response)` payload ready to ship back to the
/// client when the version is recognized but unsupported, leaving the
/// caller to send the response.
pub fn enforce_request_protocol_version(
    id: &JsonValue,
    metadata: &McpRequestMetadata,
) -> Result<(), JsonValue> {
    let Some(version) = metadata.protocol_version() else {
        return Err(crate::jsonrpc::error_response(
            id.clone(),
            -32602,
            "request _meta is missing or has malformed required fields: io.modelcontextprotocol/protocolVersion, io.modelcontextprotocol/clientInfo, io.modelcontextprotocol/clientCapabilities",
        ));
    };
    if version != rmcp::model::ProtocolVersion::V_2026_07_28 {
        return Err(unsupported_protocol_version_response(
            id.clone(),
            version.as_str(),
        ));
    }
    let missing = metadata.missing_required_keys(&rmcp::model::ProtocolVersion::V_2026_07_28);
    if !missing.is_empty() {
        return Err(crate::jsonrpc::error_response(
            id.clone(),
            -32602,
            &format!(
                "request _meta is missing or has malformed required fields: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Build the SDK-defined unsupported-protocol-version JSON-RPC error.
pub fn unsupported_protocol_version_response(
    id: impl Into<JsonValue>,
    requested: &str,
) -> JsonValue {
    crate::jsonrpc::error_response_with_data(
        id,
        UNSUPPORTED_PROTOCOL_VERSION_CODE,
        "Unsupported protocol version",
        json!({
            "supported": request_metadata_protocol_versions(),
            "requested": requested,
        }),
    )
}

/// HTTP-header validation outcome. Errors carry a JSON-RPC body so the
/// HTTP layer can ship either an HTTP 400 with the body or a 200 with
/// the JSON-RPC error — both paths exist in the stable spec.
#[derive(Clone, Debug)]
pub struct McpHttpHeaderOutcome {
    pub protocol_version: Option<String>,
}

/// Inspect standard streamable HTTP routing headers. The function is pure: it returns the
/// negotiated mode and the version pinned by the client, or a JSON-RPC
/// error body when the headers contradict the request body or name a
/// version the server does not support.
///
/// `body_method` and `body_name` are the values pulled from the JSON-RPC
/// body so the helper can detect a header/body mismatch.
pub fn negotiate_http_request<'a, F>(
    headers: F,
    body_method: Option<&str>,
    body_name: Option<&str>,
    request_id: &JsonValue,
) -> Result<McpHttpHeaderOutcome, JsonValue>
where
    F: Fn(&str) -> Option<&'a str>,
{
    let mut outcome = McpHttpHeaderOutcome {
        protocol_version: None,
    };

    if let Some(value) = headers(MCP_HEADER_PROTOCOL_VERSION) {
        if value != PROTOCOL_VERSION {
            return Err(unsupported_protocol_version_response(
                request_id.clone(),
                value,
            ));
        }
        outcome.protocol_version = Some(value.to_string());
    } else if body_method.is_some_and(|method| method != "initialize") {
        return Err(crate::jsonrpc::error_response(
            request_id.clone(),
            HEADER_MISMATCH_CODE,
            "MCP-Protocol-Version header is required",
        ));
    }

    let Some(method_header) = headers(MCP_HEADER_METHOD) else {
        return Err(crate::jsonrpc::error_response(
            request_id.clone(),
            HEADER_MISMATCH_CODE,
            "Mcp-Method header is required",
        ));
    };
    if let Some(body_method) = body_method {
        if method_header != body_method {
            return Err(crate::jsonrpc::error_response_with_data(
                request_id.clone(),
                HEADER_MISMATCH_CODE,
                "Mcp-Method header does not match request body",
                json!({
                    "headerValue": method_header,
                    "bodyMethod": body_method,
                }),
            ));
        }
    }

    if let Some(expected) = body_name.filter(|name| !name.is_empty()) {
        let Some(name_header) = headers(MCP_HEADER_NAME) else {
            return Err(crate::jsonrpc::error_response(
                request_id.clone(),
                HEADER_MISMATCH_CODE,
                "Mcp-Name header is required for this request",
            ));
        };
        let decoded = decode_header_value(name_header).ok_or_else(|| {
            crate::jsonrpc::error_response(
                request_id.clone(),
                HEADER_MISMATCH_CODE,
                "Mcp-Name header has an invalid encoded value",
            )
        })?;
        if decoded != expected {
            return Err(crate::jsonrpc::error_response_with_data(
                request_id.clone(),
                HEADER_MISMATCH_CODE,
                "Mcp-Name header does not match request body",
                json!({
                    "headerValue": name_header,
                    "bodyName": expected,
                }),
            ));
        }
    }

    Ok(outcome)
}

/// Whether a JSON-RPC protocol error must be projected as HTTP 400 by MCP.
pub fn requires_http_bad_request(response: &JsonValue) -> bool {
    matches!(
        response.pointer("/error/code").and_then(JsonValue::as_i64),
        Some(-32602)
            | Some(HEADER_MISMATCH_CODE)
            | Some(MISSING_REQUIRED_CLIENT_CAPABILITY_CODE)
            | Some(UNSUPPORTED_PROTOCOL_VERSION_CODE)
    )
}

/// Pulls the standard `Mcp-Name` header value for a request body. Stable
/// servers cross-check this against the header sent on the wire; stable
/// clients use the same helper when authoring outbound requests.
pub fn standard_name_header_value(method: &str, params: &JsonValue) -> Option<String> {
    standard_name_body_value(method, params).map(|value| encode_header_value(&value))
}

/// The unencoded request identity used when comparing decoded routing headers.
pub fn standard_name_body_value(method: &str, params: &JsonValue) -> Option<String> {
    let (_, field) = MCP_NAME_FIELDS.iter().find(|(name, _)| *name == method)?;
    params
        .get(*field)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

/// Modify a JSON-RPC result body in place to include the stable protocol's
/// per-result discriminants.
pub fn apply_result_envelope(result: &mut JsonValue, cache: Option<&McpCacheHint>) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    object
        .entry("resultType")
        .or_insert_with(|| JsonValue::String(RESULT_TYPE_COMPLETE.to_string()));
    if let Some(hint) = cache {
        if let Some(ttl) = hint.ttl_ms {
            object.insert("ttlMs".to_string(), json!(ttl));
        }
        if let Some(scope) = hint.scope {
            object.insert("cacheScope".to_string(), JsonValue::String(scope.into()));
        }
    }
}

/// Map a server method to its conservative cache hint. Read/list methods get
/// a TTL; everything else is `None`, which still routes through
/// [`apply_response_envelope`] so stable clients see `resultType`.
pub fn cache_hint_for_method(method: &str) -> Option<&'static McpCacheHint> {
    const LIST: McpCacheHint = McpCacheHint::list_default();
    const READ: McpCacheHint = McpCacheHint::read_default();
    match method {
        "tools/list" | "resources/list" | "resources/templates/list" | "prompts/list" => {
            Some(&LIST)
        }
        "resources/read" => Some(&READ),
        _ => None,
    }
}

/// Stamp the stable `resultType` and `method`'s cache hint onto a server
/// response. Error responses pass through untouched.
pub fn apply_response_envelope(mut response: JsonValue, method: &str) -> JsonValue {
    if let Some(result) = response.get_mut("result") {
        apply_result_envelope(result, cache_hint_for_method(method));
    }
    response
}

/// Answer one page of a list method: `items[cursor..]` under `result_key`,
/// with `nextCursor` when more remain. A malformed cursor is `-32602`.
pub fn list_response(
    id: JsonValue,
    method: &str,
    result_key: &str,
    params: &JsonValue,
    items: Vec<JsonValue>,
) -> JsonValue {
    let page = match mcp_list_page(params, items.len(), method) {
        Ok(page) => page,
        Err(error) => return crate::jsonrpc::error_response(id, -32602, &error),
    };
    let page_items = items
        .into_iter()
        .skip(page.start)
        .take(page.end - page.start)
        .collect::<Vec<_>>();
    let mut result = serde_json::Map::new();
    result.insert(result_key.to_string(), JsonValue::Array(page_items));
    if let Some(next_cursor) = page.next_cursor {
        result.insert("nextCursor".to_string(), JsonValue::String(next_cursor));
    }
    crate::jsonrpc::response(id, JsonValue::Object(result))
}

/// Conservative cache hint surfaced on stable results. Servers can override
/// the defaults per handler when they have a better answer; clients fall
/// back to the constants in [`DEFAULT_LIST_CACHE_TTL_MS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpCacheHint {
    pub ttl_ms: Option<u64>,
    pub scope: Option<&'static str>,
}

impl McpCacheHint {
    pub const fn list_default() -> Self {
        Self {
            ttl_ms: Some(DEFAULT_LIST_CACHE_TTL_MS),
            scope: Some(DEFAULT_LIST_CACHE_SCOPE),
        }
    }

    pub const fn read_default() -> Self {
        Self {
            ttl_ms: Some(DEFAULT_READ_CACHE_TTL_MS),
            scope: Some(DEFAULT_READ_CACHE_SCOPE),
        }
    }

    pub const fn none() -> Self {
        Self {
            ttl_ms: None,
            scope: None,
        }
    }

    /// Extract a cache hint from a stable result body. Returns `None` when
    /// neither `ttlMs` nor a recognized `cacheScope` is present; unknown
    /// scopes are silently dropped so we stay forward-compatible.
    pub fn from_result(result: &JsonValue) -> Option<Self> {
        let ttl_ms = result.get("ttlMs").and_then(JsonValue::as_u64);
        let scope = result
            .get("cacheScope")
            .and_then(JsonValue::as_str)
            .and_then(Self::canonical_scope);
        if ttl_ms.is_none() && scope.is_none() {
            return None;
        }
        Some(Self { ttl_ms, scope })
    }

    fn canonical_scope(value: &str) -> Option<&'static str> {
        match value {
            "public" => Some("public"),
            "private" => Some("private"),
            _ => None,
        }
    }

    pub fn to_json_object(&self) -> serde_json::Map<String, JsonValue> {
        let mut entry = serde_json::Map::new();
        if let Some(ttl_ms) = self.ttl_ms {
            entry.insert("ttlMs".to_string(), json!(ttl_ms));
        }
        if let Some(scope) = self.scope {
            entry.insert("cacheScope".to_string(), JsonValue::String(scope.into()));
        }
        entry
    }
}

/// Build a JSON object mapping method names to their recorded MCP cache
/// hints. Empty input yields an empty object.
pub fn cache_hints_to_json<'a, I>(hints: I) -> JsonValue
where
    I: IntoIterator<Item = (&'a String, &'a McpCacheHint)>,
{
    let mut object = serde_json::Map::new();
    for (method, hint) in hints {
        object.insert(method.clone(), JsonValue::Object(hint.to_json_object()));
    }
    JsonValue::Object(object)
}

/// Build the canonical `server/discover` result both server surfaces
/// share. Callers supply their advertised capabilities and serverInfo;
/// the helper handles `resultType`, `supportedVersions`, instructions,
/// and any stable envelope fields.
pub fn server_discover_result(
    capabilities: JsonValue,
    server_info: JsonValue,
    instructions: Option<&str>,
) -> JsonValue {
    let mut result = json!({
        "resultType": RESULT_TYPE_COMPLETE,
        "supportedVersions": request_metadata_protocol_versions(),
        "capabilities": capabilities,
        "ttlMs": 0,
        "cacheScope": "private",
        "_meta": {
            (MCP_META_KEY_SERVER_INFO): server_info,
        },
    });
    if let Some(instructions) = instructions {
        result["instructions"] = JsonValue::String(instructions.to_string());
    }
    result
}

pub fn explicit_unsupported_method_response(
    id: impl Into<JsonValue>,
    method: &str,
) -> Option<JsonValue> {
    let (feature, role, reason) = match method {
        METHOD_SAMPLING_CREATE_MESSAGE => (
            "sampling",
            "client",
            "MCP sampling is an embedded input request in stable multi-round-trip results; it cannot be called as a top-level request on an MCP server endpoint.",
        ),
        METHOD_ELICITATION_CREATE => (
            "elicitation",
            "client",
            "MCP elicitation is an embedded input request in stable multi-round-trip results; it cannot be called as a top-level request on an MCP server endpoint.",
        ),
        "subscriptions/listen" => (
            "subscriptions",
            "server",
            "Harn does not advertise or implement the request-scoped notification stream.",
        ),
        _ => return None,
    };
    Some(crate::jsonrpc::error_response_with_data(
        id,
        -32601,
        &format!("Unsupported MCP client-bound method: {method}"),
        json!({
            "type": "mcp.unsupportedFeature",
            "protocolVersion": PROTOCOL_VERSION,
            "method": method,
            "feature": feature,
            "role": role,
            "status": "unsupported",
            "reason": reason,
        }),
    ))
}

pub fn client_supports_tasks(params: &JsonValue) -> bool {
    params
        .pointer("/_meta/io.modelcontextprotocol~1clientCapabilities/extensions/io.modelcontextprotocol~1tasks")
        .is_some()
}

pub fn is_task_method(method: &str) -> bool {
    matches!(
        method,
        METHOD_TASKS_GET | METHOD_TASKS_UPDATE | METHOD_TASKS_CANCEL
    )
}

pub fn missing_tasks_capability_response(id: impl Into<JsonValue>) -> JsonValue {
    crate::jsonrpc::error_response_with_data(
        id,
        MISSING_REQUIRED_CLIENT_CAPABILITY_CODE,
        "Missing required client capability",
        json!({
            "requiredCapabilities": {
                "extensions": {TASKS_EXTENSION_ID: {}}
            }
        }),
    )
}

pub fn tasks_capability() -> JsonValue {
    json!({
        TASKS_EXTENSION_ID: {}
    })
}

pub fn completions_capability() -> JsonValue {
    json!({})
}

pub fn completion_result(
    id: impl Into<JsonValue>,
    candidates: Vec<String>,
    value: &str,
) -> JsonValue {
    crate::jsonrpc::response(
        id,
        json!({ "completion": completion_payload(candidates, value) }),
    )
}

pub fn completion_payload(candidates: Vec<String>, value: &str) -> JsonValue {
    let needle = value.to_ascii_lowercase();
    let mut seen = std::collections::BTreeSet::new();
    let mut ranked = candidates
        .into_iter()
        .filter_map(|candidate| {
            let candidate = candidate.trim().to_string();
            if candidate.is_empty() || !seen.insert(candidate.clone()) {
                return None;
            }
            let haystack = candidate.to_ascii_lowercase();
            if !needle.is_empty() && !haystack.contains(&needle) {
                return None;
            }
            let rank = i32::from(!(needle.is_empty() || haystack.starts_with(&needle)));
            Some((rank, haystack, candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let total = ranked.len();
    let values = ranked
        .into_iter()
        .take(MCP_COMPLETION_MAX_VALUES)
        .map(|(_, _, candidate)| candidate)
        .collect::<Vec<_>>();
    json!({
        "values": values,
        "total": total,
        "hasMore": total > MCP_COMPLETION_MAX_VALUES,
    })
}

pub fn mcp_list_page_size() -> usize {
    mcp_list_page_size_from_env(std::env::var(MCP_LIST_PAGE_SIZE_ENV).ok().as_deref())
}

fn mcp_list_page_size_from_env(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_MCP_LIST_PAGE_SIZE)
}

pub fn encode_mcp_list_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(offset.to_string().as_bytes())
}

pub fn mcp_list_page(
    params: &JsonValue,
    total_len: usize,
    method: &str,
) -> Result<McpListPage, String> {
    let offset = parse_mcp_list_cursor(params, method)?;
    let page_size = mcp_list_page_size();
    let start = offset.min(total_len);
    let end = start.saturating_add(page_size).min(total_len);
    let next_cursor = (end < total_len).then(|| encode_mcp_list_cursor(end));
    Ok(McpListPage {
        start,
        end,
        next_cursor,
    })
}

fn parse_mcp_list_cursor(params: &JsonValue, method: &str) -> Result<usize, String> {
    let Some(cursor) = params.get("cursor") else {
        return Ok(0);
    };
    let Some(cursor) = cursor.as_str() else {
        return Err(format!("invalid {method} cursor"));
    };
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cursor)
        .map_err(|_| format!("invalid {method} cursor"))?;
    let decoded = String::from_utf8(bytes).map_err(|_| format!("invalid {method} cursor"))?;
    decoded
        .parse::<usize>()
        .map_err(|_| format!("invalid {method} cursor"))
}

#[cfg(test)]
mod tests;
