//! Harn policy layered on the official MCP Rust SDK protocol model.
//!
//! `rmcp` owns protocol versions, standard error codes, typed messages, and
//! lifecycle semantics. This module retains only Harn policy that is not part
//! of MCP itself: pagination limits, cache defaults, and conversion helpers for
//! the VM's JSON-valued interface.

use serde_json::{json, Value as JsonValue};

/// Stable MCP version used by Harn's discover/per-request lifecycle.
///
/// Keep this literal mechanically checked against `rmcp` below because public
/// Harn projections require a `&'static str` while the SDK exposes a typed
/// [`rmcp::model::ProtocolVersion`].
pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const METHOD_SERVER_DISCOVER: &str = "server/discover";
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

    if !name.is_empty() {
        let bytes = name.as_bytes();
        if !bytes[0].is_ascii_alphanumeric()
            || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.'))
        {
            return Err("has an invalid metadata name".to_string());
        }
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

    pub(crate) fn is_ready_for_notifications(&self) -> bool {
        self.notifications_ready
    }

    pub(crate) fn accept_initialized_notification(&mut self) {
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
        if name_header != expected {
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
    match method {
        "tools/call" | "prompts/get" => params
            .get("name")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        "resources/read" => params
            .get("uri")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        METHOD_TASKS_GET | METHOD_TASKS_UPDATE | METHOD_TASKS_CANCEL => params
            .get("taskId")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        _ => None,
    }
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
            "io.modelcontextprotocol/serverInfo": server_info,
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
mod tests {
    use super::*;

    #[test]
    fn protocol_registry_matches_official_sdk() {
        assert_eq!(
            PROTOCOL_VERSION,
            rmcp::model::ProtocolVersion::V_2026_07_28.as_str()
        );
        assert_eq!(
            sdk_protocol_versions(),
            rmcp::model::ProtocolVersion::KNOWN_VERSIONS
                .iter()
                .map(rmcp::model::ProtocolVersion::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            request_metadata_protocol_versions(),
            rmcp::model::ProtocolVersion::KNOWN_VERSIONS
                .iter()
                .filter(|version| *version >= &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
                .map(rmcp::model::ProtocolVersion::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(UNSUPPORTED_PROTOCOL_VERSION_CODE, -32022);
        assert_eq!(MISSING_REQUIRED_CLIENT_CAPABILITY_CODE, -32021);
        assert_eq!(HEADER_MISMATCH_CODE, -32020);
    }

    #[test]
    fn initialize_negotiates_every_sdk_released_version() {
        for protocol_version in rmcp::model::ProtocolVersion::KNOWN_VERSIONS
            .iter()
            .filter(|version| *version < &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
        {
            let outcome = negotiate_initialize(
                &json!({
                    "protocolVersion": protocol_version.as_str(),
                    "capabilities": {},
                    "clientInfo": {"name": "codex-mcp-client", "version": "test"},
                }),
                json!({"tools": {}}),
                json!({"name": "harn", "version": "test"}),
                Some("test server"),
            )
            .expect("SDK-supported initialize version should negotiate");
            assert_eq!(&outcome.protocol_version, protocol_version);
            assert_eq!(outcome.client_identity, "codex-mcp-client/test");
            assert_eq!(
                outcome.result["protocolVersion"],
                json!(protocol_version.as_str())
            );
            assert_eq!(outcome.result["capabilities"]["tools"], json!({}));
            assert_eq!(outcome.result["serverInfo"]["name"], json!("harn"));
            assert_eq!(outcome.result["instructions"], json!("test server"));

            let typed: rmcp::model::InitializeResult = serde_json::from_value(outcome.result)
                .expect("initialize response must remain SDK-typed");
            assert_eq!(&typed.protocol_version, protocol_version);
        }

        let modern_initialize = negotiate_initialize(
            &json!({
                "protocolVersion": rmcp::model::ProtocolVersion::STANDARD_HEADERS.as_str(),
                "capabilities": {},
                "clientInfo": {"name": "codex-mcp-client", "version": "test"},
            }),
            json!({"tools": {}}),
            json!({"name": "harn", "version": "test"}),
            None,
        )
        .expect("modern initialize request should negotiate a released fallback");
        assert_eq!(
            modern_initialize.protocol_version,
            rmcp::model::ProtocolVersion::LATEST
        );
    }

    #[test]
    fn initialized_session_keeps_its_version_when_request_meta_has_a_progress_token() {
        let mut session = McpServerSession::default();
        session
            .initialize(
                &json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "codex-mcp-client", "version": "test"},
                }),
                json!({"tools": {}}),
                json!({"name": "harn", "version": "test"}),
                None,
            )
            .expect("initialize should negotiate");

        let profile = session
            .accept_request(
                &json!(2),
                "tools/call",
                &json!({"_meta": {"progressToken": "codex-proof"}}),
            )
            .expect("released request metadata should use the initialized version");
        assert!(!profile.uses_result_envelope());
    }

    #[test]
    fn released_session_waits_for_initialized_notification_before_server_notifications() {
        let mut session = McpServerSession::default();
        session.accept_initialized_notification();
        assert!(!session.is_ready_for_notifications());
        session
            .initialize(
                &json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "codex-mcp-client", "version": "test"},
                }),
                json!({"tools": {"listChanged": true}}),
                json!({"name": "harn", "version": "test"}),
                None,
            )
            .expect("initialize should negotiate");

        assert!(!session.is_ready_for_notifications());
        session.accept_initialized_notification();
        assert!(session.is_ready_for_notifications());
    }

    #[test]
    fn completion_payload_dedupes_and_ranks_prefix_matches() {
        let response = completion_result(
            json!(1),
            vec![
                "typescript".to_string(),
                "rust".to_string(),
                "ruby".to_string(),
                "rust".to_string(),
            ],
            "ru",
        );
        assert_eq!(
            response["result"]["completion"]["values"],
            json!(["ruby", "rust"])
        );
        assert_eq!(response["result"]["completion"]["total"], json!(2));
        assert_eq!(response["result"]["completion"]["hasMore"], json!(false));
    }

    #[test]
    fn task_capability_uses_the_stable_extension_map() {
        assert!(client_supports_tasks(&json!({
            "_meta": {
                MCP_META_KEY_CLIENT_CAPABILITIES: {
                    "extensions": {TASKS_EXTENSION_ID: {}}
                }
            }
        })));
        assert!(!client_supports_tasks(&json!({})));
    }

    #[test]
    fn application_metadata_keys_follow_mcp_grammar_and_reservations() {
        for valid in [
            "progressToken",
            "com.example/tool-version",
            "com.harnlang/toolContract",
        ] {
            validate_application_meta_key(valid).expect(valid);
        }
        for invalid in [
            "vendor.example/version/extra",
            "1example.vendor/version",
            "com..vendor/version",
            "com.mcp.tools/version",
            "io.modelcontextprotocol/private",
            "com.example/-version",
        ] {
            assert!(
                validate_application_meta_key(invalid).is_err(),
                "invalid MCP metadata key accepted: {invalid}"
            );
        }
    }

    #[test]
    fn task_protocol_shapes_match_latest_spec_names() {
        assert_eq!(mcp_task_status_wire_name(McpTaskStatus::Working), "working");
        assert_eq!(
            mcp_task_status_wire_name(McpTaskStatus::InputRequired),
            "input_required"
        );
        assert!(McpTaskStatus::Completed.is_terminal());
        assert_eq!(tasks_capability()[TASKS_EXTENSION_ID], json!({}));
    }

    #[test]
    fn mcp_list_page_uses_default_size_and_next_cursor() {
        let page = mcp_list_page(&json!({}), 105, "tools/list").unwrap();
        assert_eq!(page.start, 0);
        assert_eq!(page.end, DEFAULT_MCP_LIST_PAGE_SIZE);
        assert_eq!(
            page.next_cursor,
            Some(encode_mcp_list_cursor(DEFAULT_MCP_LIST_PAGE_SIZE))
        );

        let next = mcp_list_page(
            &json!({"cursor": page.next_cursor.unwrap()}),
            105,
            "tools/list",
        )
        .unwrap();
        assert_eq!(next.start, DEFAULT_MCP_LIST_PAGE_SIZE);
        assert_eq!(next.end, 105);
        assert_eq!(next.next_cursor, None);
    }

    #[test]
    fn mcp_list_page_size_parses_positive_env_override() {
        assert_eq!(mcp_list_page_size_from_env(Some("2")), 2);
        assert_eq!(
            mcp_list_page_size_from_env(Some("0")),
            DEFAULT_MCP_LIST_PAGE_SIZE
        );
        assert_eq!(
            mcp_list_page_size_from_env(Some("nope")),
            DEFAULT_MCP_LIST_PAGE_SIZE
        );
        assert_eq!(
            mcp_list_page_size_from_env(None),
            DEFAULT_MCP_LIST_PAGE_SIZE
        );
    }

    #[test]
    fn mcp_list_page_rejects_malformed_cursor() {
        let err = mcp_list_page(&json!({"cursor": "not-base64"}), 5, "resources/list")
            .expect_err("malformed cursor should fail");
        assert_eq!(err, "invalid resources/list cursor");
    }

    #[test]
    fn stable_metadata_round_trips_through_meta_block() {
        let params = json!({
            "_meta": {
                MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
                MCP_META_KEY_CLIENT_INFO: {"name": "harn", "version": "x"},
                MCP_META_KEY_CLIENT_CAPABILITIES: {"roots": {}},
            }
        });
        let meta = parse_request_metadata(&params);
        assert_eq!(
            meta.protocol_version()
                .as_ref()
                .map(|version| version.as_str()),
            Some(PROTOCOL_VERSION)
        );
        assert_eq!(
            serde_json::to_value(meta.client_info()).unwrap(),
            json!({"name": "harn", "version": "x"})
        );
        assert_eq!(
            serde_json::to_value(meta.client_capabilities()).unwrap(),
            json!({"roots": {}})
        );
        enforce_request_protocol_version(&json!(1), &meta).unwrap();
    }

    #[test]
    fn stable_metadata_is_required() {
        let meta = parse_request_metadata(&json!({}));
        assert_eq!(meta, McpRequestMetadata::default());
        let error = enforce_request_protocol_version(&json!(1), &meta).unwrap_err();
        assert_eq!(error["error"]["code"], json!(-32602));
    }

    #[test]
    fn enforce_request_protocol_version_rejects_unknown_version() {
        let meta = parse_request_metadata(&json!({
            "_meta": {MCP_META_KEY_PROTOCOL_VERSION: "2099-01-01"}
        }));
        let id = json!(7);
        let err =
            enforce_request_protocol_version(&id, &meta).expect_err("unknown version should error");
        assert_eq!(err["id"], id);
        assert_eq!(
            err["error"]["code"],
            json!(UNSUPPORTED_PROTOCOL_VERSION_CODE)
        );
        assert_eq!(err["error"]["data"]["requested"], json!("2099-01-01"));
        let supported = err["error"]["data"]["supported"].as_array().unwrap();
        assert!(supported.contains(&json!(PROTOCOL_VERSION)));
    }

    #[test]
    fn enforce_request_protocol_version_accepts_stable_metadata() {
        let meta = parse_request_metadata(&json!({
            "_meta": {
                MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
                MCP_META_KEY_CLIENT_INFO: {"name": "harn", "version": "x"},
                MCP_META_KEY_CLIENT_CAPABILITIES: {},
            }
        }));
        enforce_request_protocol_version(&json!(1), &meta).unwrap();
    }

    #[test]
    fn enforce_request_protocol_version_uses_sdk_required_metadata_validation() {
        let meta = parse_request_metadata(&json!({
            "_meta": {MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION}
        }));
        let error = enforce_request_protocol_version(&json!(1), &meta)
            .expect_err("stable requests require typed client capabilities");
        assert_eq!(error["error"]["code"], json!(-32602));
        assert!(error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(MCP_META_KEY_CLIENT_CAPABILITIES)));
    }

    #[test]
    fn enforce_request_protocol_version_rejects_old_versions() {
        let meta = parse_request_metadata(&json!({
            "_meta": {
                MCP_META_KEY_PROTOCOL_VERSION: "2025-06-18",
                MCP_META_KEY_CLIENT_INFO: {"name": "old", "version": "1"},
                MCP_META_KEY_CLIENT_CAPABILITIES: {},
            }
        }));
        let error = enforce_request_protocol_version(&json!(1), &meta).unwrap_err();
        assert_eq!(
            error["error"]["code"],
            json!(UNSUPPORTED_PROTOCOL_VERSION_CODE)
        );
    }

    #[test]
    fn negotiate_standard_http_headers_detects_stable_protocol_header() {
        let headers = std::collections::HashMap::from([
            (
                MCP_HEADER_PROTOCOL_VERSION.to_string(),
                PROTOCOL_VERSION.to_string(),
            ),
            (MCP_HEADER_METHOD.to_string(), "tools/list".to_string()),
        ]);
        let outcome = negotiate_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/list"),
            None,
            &json!(1),
        )
        .unwrap();
        assert_eq!(outcome.protocol_version.as_deref(), Some(PROTOCOL_VERSION));
    }

    #[test]
    fn negotiate_standard_http_headers_rejects_method_body_mismatch() {
        let headers = std::collections::HashMap::from([
            (
                MCP_HEADER_PROTOCOL_VERSION.to_string(),
                PROTOCOL_VERSION.to_string(),
            ),
            (MCP_HEADER_METHOD.to_string(), "tools/list".to_string()),
        ]);
        let err = negotiate_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/call"),
            None,
            &json!(2),
        )
        .expect_err("header/body mismatch must error");
        assert_eq!(err["error"]["code"], json!(HEADER_MISMATCH_CODE));
        assert_eq!(err["error"]["data"]["headerValue"], json!("tools/list"));
        assert_eq!(err["error"]["data"]["bodyMethod"], json!("tools/call"));
    }

    #[test]
    fn negotiate_standard_http_headers_rejects_name_body_mismatch() {
        let headers = std::collections::HashMap::from([
            (
                MCP_HEADER_PROTOCOL_VERSION.to_string(),
                PROTOCOL_VERSION.to_string(),
            ),
            (MCP_HEADER_METHOD.to_string(), "tools/call".to_string()),
            (MCP_HEADER_NAME.to_string(), "wrong".to_string()),
        ]);
        let err = negotiate_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/call"),
            Some("right"),
            &json!(3),
        )
        .expect_err("name mismatch must error");
        assert_eq!(err["error"]["code"], json!(HEADER_MISMATCH_CODE));
        assert_eq!(err["error"]["data"]["bodyName"], json!("right"));
    }

    #[test]
    fn negotiate_standard_http_headers_rejects_missing_required_headers() {
        let none = std::collections::HashMap::<String, String>::new();
        let missing_protocol = negotiate_http_request(
            |key| none.get(key).map(String::as_str),
            Some("tools/list"),
            None,
            &json!(4),
        )
        .expect_err("modern requests require a protocol header");
        assert_eq!(
            missing_protocol["error"]["code"],
            json!(HEADER_MISMATCH_CODE)
        );

        let headers = std::collections::HashMap::from([
            (
                MCP_HEADER_PROTOCOL_VERSION.to_string(),
                PROTOCOL_VERSION.to_string(),
            ),
            (MCP_HEADER_METHOD.to_string(), "tools/call".to_string()),
        ]);
        let missing_name = negotiate_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/call"),
            Some("weather"),
            &json!(5),
        )
        .expect_err("named requests require Mcp-Name");
        assert_eq!(missing_name["error"]["code"], json!(HEADER_MISMATCH_CODE));
    }

    #[test]
    fn protocol_validation_errors_require_http_bad_request() {
        for code in [
            -32602,
            HEADER_MISMATCH_CODE,
            MISSING_REQUIRED_CLIENT_CAPABILITY_CODE,
            UNSUPPORTED_PROTOCOL_VERSION_CODE,
        ] {
            assert!(requires_http_bad_request(&json!({"error": {"code": code}})));
        }
        assert!(!requires_http_bad_request(
            &json!({"error": {"code": -32601}})
        ));
        assert!(!requires_http_bad_request(&json!({"result": {}})));
    }

    #[test]
    fn stable_request_identity_never_leaks_from_a_previous_request() {
        let mut session = McpServerSession::default();
        let with_identity = json!({
            "_meta": {
                MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
                MCP_META_KEY_CLIENT_INFO: {"name": "first", "version": "1"},
                MCP_META_KEY_CLIENT_CAPABILITIES: {},
            }
        });
        session
            .accept_request(&json!(1), METHOD_SERVER_DISCOVER, &with_identity)
            .unwrap();
        assert_eq!(session.client_identity(), "first/1");

        let without_identity = json!({
            "_meta": {
                MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
                MCP_META_KEY_CLIENT_CAPABILITIES: {},
            }
        });
        session
            .accept_request(&json!(2), METHOD_SERVER_DISCOVER, &without_identity)
            .unwrap();
        assert_eq!(session.client_identity(), "unknown");
    }

    #[test]
    fn standard_name_header_value_extracts_method_subject() {
        assert_eq!(
            standard_name_header_value("tools/call", &json!({"name": "demo"})),
            Some("demo".to_string())
        );
        assert_eq!(
            standard_name_header_value("prompts/get", &json!({"name": "p"})),
            Some("p".to_string())
        );
        assert_eq!(
            standard_name_header_value("resources/read", &json!({"uri": "harn://x"})),
            Some("harn://x".to_string())
        );
        for method in [METHOD_TASKS_GET, METHOD_TASKS_UPDATE, METHOD_TASKS_CANCEL] {
            assert_eq!(
                standard_name_header_value(method, &json!({"taskId": "task-123"})),
                Some("task-123".to_string())
            );
        }
        assert_eq!(standard_name_header_value("tools/list", &json!({})), None);
    }

    #[test]
    fn missing_task_capability_uses_the_extension_error_contract() {
        let response = missing_tasks_capability_response(json!(7));
        assert_eq!(response["error"]["code"], -32021);
        assert_eq!(
            response["error"]["data"]["requiredCapabilities"]["extensions"][TASKS_EXTENSION_ID],
            json!({})
        );
    }

    #[test]
    fn apply_result_envelope_adds_result_type_and_cache() {
        let mut stable = json!({"tools": []});
        apply_result_envelope(&mut stable, Some(&McpCacheHint::list_default()));
        assert_eq!(stable["resultType"], json!(RESULT_TYPE_COMPLETE));
        assert_eq!(stable["ttlMs"], json!(DEFAULT_LIST_CACHE_TTL_MS));
        assert_eq!(stable["cacheScope"], json!(DEFAULT_LIST_CACHE_SCOPE));
    }

    #[test]
    fn apply_result_envelope_preserves_caller_provided_result_type() {
        let mut result = json!({"resultType": RESULT_TYPE_INPUT_REQUIRED});
        apply_result_envelope(&mut result, None);
        assert_eq!(result["resultType"], json!(RESULT_TYPE_INPUT_REQUIRED));
    }

    #[test]
    fn server_discover_result_advertises_request_metadata_versions() {
        let discover = server_discover_result(
            json!({"tools": {}}),
            json!({"name": "harn", "version": "x"}),
            Some("hello"),
        );
        assert_eq!(discover["resultType"], json!(RESULT_TYPE_COMPLETE));
        assert_eq!(discover["ttlMs"], json!(0));
        assert_eq!(discover["cacheScope"], json!("private"));
        assert_eq!(
            discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            json!("harn")
        );
        let supported = discover["supportedVersions"].as_array().unwrap();
        assert_eq!(supported, &[json!(PROTOCOL_VERSION)]);
        assert_eq!(discover["instructions"], json!("hello"));

        let typed: rmcp::model::DiscoverResult = serde_json::from_value(discover)
            .expect("Harn discovery result must match the official SDK type");
        assert_eq!(typed.result_type, rmcp::model::ResultType::COMPLETE);
        assert_eq!(typed.ttl_ms, 0);
        assert_eq!(
            typed.server_info().map(|info| info.name),
            Some("harn".to_string())
        );
    }

    #[test]
    fn subscriptions_listen_is_explicitly_unsupported() {
        let response = explicit_unsupported_method_response(json!(7), "subscriptions/listen")
            .expect("known unsupported stable method");
        assert_eq!(response["error"]["code"], json!(-32601));
        assert_eq!(
            response["error"]["data"]["type"],
            json!("mcp.unsupportedFeature")
        );
        assert_eq!(
            response["error"]["data"]["protocolVersion"],
            json!(PROTOCOL_VERSION)
        );
    }

    #[test]
    fn stable_input_methods_are_rejected_as_top_level_server_calls() {
        for method in [METHOD_SAMPLING_CREATE_MESSAGE, METHOD_ELICITATION_CREATE] {
            let response = explicit_unsupported_method_response(json!(7), method)
                .expect("input method has an explicit boundary error");
            assert_eq!(response["error"]["code"], json!(-32601));
            assert_eq!(response["error"]["data"]["method"], json!(method));
        }
    }
}
