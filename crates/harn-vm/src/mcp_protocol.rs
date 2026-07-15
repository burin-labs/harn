//! Shared MCP protocol-version and feature-gap helpers.

use serde_json::{json, Value as JsonValue};

/// Stable, production MCP protocol version that Harn defaults to.
pub const PROTOCOL_VERSION: &str = "2025-11-25";
/// Prior stable MCP protocol version with the same initialize/session shape
/// Harn uses for [`PROTOCOL_VERSION`]. Kept for clients such as Codex that
/// still negotiate this released version.
pub const LEGACY_2025_06_18_PROTOCOL_VERSION: &str = "2025-06-18";
/// Original public MCP protocol version. It uses the same basic initialize /
/// initialized lifecycle shape Harn needs for stdio tool servers, so keep it as
/// a legacy compatibility profile for clients pinned to the first release.
pub const LEGACY_2024_11_05_PROTOCOL_VERSION: &str = "2024-11-05";
/// RC profile published alongside the stable version. Both clients and
/// servers opt into this profile per request; the runtime never assumes
/// a connection is RC-only unless the client signals it via metadata or
/// HTTP headers.
pub const DRAFT_PROTOCOL_VERSION: &str = "DRAFT-2026-v1";

pub const METHOD_SERVER_DISCOVER: &str = "server/discover";
pub const METHOD_TASKS_GET: &str = "tasks/get";
pub const METHOD_TASKS_RESULT: &str = "tasks/result";
pub const METHOD_TASKS_LIST: &str = "tasks/list";
pub const METHOD_TASKS_CANCEL: &str = "tasks/cancel";
pub const METHOD_COMPLETION_COMPLETE: &str = "completion/complete";
pub const METHOD_SAMPLING_CREATE_MESSAGE: &str = "sampling/createMessage";
pub const METHOD_ELICITATION_CREATE: &str = "elicitation/create";
pub const METHOD_TASK_STATUS_NOTIFICATION: &str = "notifications/tasks/status";
pub const METHOD_ROOTS_LIST: &str = "roots/list";
pub const METHOD_ROOTS_LIST_CHANGED_NOTIFICATION: &str = "notifications/roots/list_changed";
pub const METHOD_LOGGING_SET_LEVEL: &str = "logging/setLevel";
pub const METHOD_LOGGING_MESSAGE_NOTIFICATION: &str = "notifications/message";
pub const RELATED_TASK_META_KEY: &str = "io.modelcontextprotocol/related-task";

/// RC request metadata keys carried inside `params._meta` so the server
/// can identify the client's protocol target without a sticky session.
pub const RC_META_KEY_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const RC_META_KEY_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub const RC_META_KEY_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

/// RC HTTP headers expected on streamable POSTs.
pub const RC_HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";
pub const RC_HEADER_METHOD: &str = "mcp-method";
pub const RC_HEADER_NAME: &str = "mcp-name";
/// Legacy HTTP session header that pre-RC clients and servers carry. The
/// RC modern profile is stateless and never emits this header.
pub const MCP_SESSION_HEADER_LEGACY: &str = "mcp-session-id";

/// `resultType` discriminants exposed to RC clients on every response
/// result body. Legacy clients never see this field, which preserves the
/// pre-RC wire format byte-for-byte.
pub const RESULT_TYPE_COMPLETE: &str = "complete";
pub const RESULT_TYPE_INPUT_REQUIRED: &str = "input_required";

/// JSON-RPC error code reserved by the RC for unsupported protocol
/// version negotiation. Returned with `data.supported` listing the
/// versions the peer accepts.
pub const UNSUPPORTED_PROTOCOL_VERSION_CODE: i64 = -32004;

pub const DEFAULT_TASK_POLL_INTERVAL_MS: u64 = 250;
pub const DEFAULT_MCP_LIST_PAGE_SIZE: usize = 100;
pub const MCP_LIST_PAGE_SIZE_ENV: &str = "HARN_MCP_LIST_PAGE_SIZE";

/// Conservative cache hints emitted with list/read results when an RC
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTaskStatus {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl McpTaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpToolTaskSupport {
    Required,
    Optional,
    Forbidden,
}

impl McpToolTaskSupport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Forbidden => "forbidden",
        }
    }
}

/// Identifies which MCP profile a single request targets. The mode is
/// negotiated per request from `_meta` or HTTP headers, never sticky.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpProtocolMode {
    /// `2025-11-25` initialize/notify flow with session-id stickiness.
    #[default]
    Legacy,
    /// RC profile: per-request metadata, optional cache hints, explicit
    /// `resultType` discriminants, no session stickiness.
    Modern,
}

impl McpProtocolMode {
    pub fn is_modern(self) -> bool {
        matches!(self, Self::Modern)
    }

    pub fn default_protocol_version(self) -> &'static str {
        match self {
            Self::Legacy => PROTOCOL_VERSION,
            Self::Modern => DRAFT_PROTOCOL_VERSION,
        }
    }
}

/// Returns the protocol versions this Harn build understands when
/// peers ask via `server/discover` or fail with `-32004`.
pub fn supported_protocol_versions() -> &'static [&'static str] {
    &[
        DRAFT_PROTOCOL_VERSION,
        PROTOCOL_VERSION,
        LEGACY_2025_06_18_PROTOCOL_VERSION,
        LEGACY_2024_11_05_PROTOCOL_VERSION,
    ]
}

pub fn is_supported_protocol_version(version: &str) -> bool {
    supported_protocol_versions().contains(&version)
}

/// Parsed per-request RC metadata. All fields are optional; legacy
/// requests yield an empty struct.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpRequestMetadata {
    pub protocol_version: Option<String>,
    pub client_info: Option<JsonValue>,
    pub client_capabilities: Option<JsonValue>,
}

impl McpRequestMetadata {
    pub fn mode(&self) -> McpProtocolMode {
        match self.protocol_version.as_deref() {
            Some(DRAFT_PROTOCOL_VERSION) => McpProtocolMode::Modern,
            _ => McpProtocolMode::Legacy,
        }
    }
}

/// Extract RC metadata from a request's `params._meta` block. Unknown
/// keys are ignored so this stays forward-compatible with future RC
/// drafts.
pub fn parse_request_metadata(params: &JsonValue) -> McpRequestMetadata {
    let Some(meta) = params.get("_meta").and_then(JsonValue::as_object) else {
        return McpRequestMetadata::default();
    };
    let protocol_version = meta
        .get(RC_META_KEY_PROTOCOL_VERSION)
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    let client_info = meta.get(RC_META_KEY_CLIENT_INFO).cloned();
    let client_capabilities = meta.get(RC_META_KEY_CLIENT_CAPABILITIES).cloned();
    McpRequestMetadata {
        protocol_version,
        client_info,
        client_capabilities,
    }
}

/// Validate that a request's metadata targets a supported version.
/// Returns an `Err(error_response)` payload ready to ship back to the
/// client when the version is recognized but unsupported, leaving the
/// caller to send the response. `Ok(None)` means legacy; `Ok(Some(meta))`
/// means RC.
pub fn enforce_request_protocol_version(
    id: &JsonValue,
    metadata: &McpRequestMetadata,
) -> Result<Option<McpProtocolMode>, JsonValue> {
    let Some(version) = metadata.protocol_version.as_deref() else {
        return Ok(None);
    };
    if !is_supported_protocol_version(version) {
        return Err(unsupported_protocol_version_response(id.clone(), version));
    }
    Ok(Some(metadata.mode()))
}

/// Build the `-32004 Unsupported protocol version` JSON-RPC error the
/// RC expects so a client can retry with a mutually-supported version.
pub fn unsupported_protocol_version_response(
    id: impl Into<JsonValue>,
    requested: &str,
) -> JsonValue {
    crate::jsonrpc::error_response_with_data(
        id,
        UNSUPPORTED_PROTOCOL_VERSION_CODE,
        "Unsupported protocol version",
        json!({
            "supported": supported_protocol_versions(),
            "requested": requested,
        }),
    )
}

/// HTTP-header validation outcome. Errors carry a JSON-RPC body so the
/// HTTP layer can ship either an HTTP 400 with the body or a 200 with
/// the JSON-RPC error — both paths exist in the RC spec.
#[derive(Clone, Debug)]
pub struct RcHttpHeaderOutcome {
    pub mode: McpProtocolMode,
    pub protocol_version: Option<String>,
}

/// Inspect the streamable HTTP request headers for the RC signals the
/// server has to validate. The function is pure: it returns the
/// negotiated mode and the version pinned by the client, or a JSON-RPC
/// error body when the headers contradict the request body or name a
/// version the server does not support.
///
/// `body_method` and `body_name` are the values pulled from the JSON-RPC
/// body so the helper can detect a header/body mismatch.
pub fn negotiate_rc_http_request<'a, F>(
    headers: F,
    body_method: Option<&str>,
    body_name: Option<&str>,
    request_id: &JsonValue,
) -> Result<RcHttpHeaderOutcome, JsonValue>
where
    F: Fn(&str) -> Option<&'a str>,
{
    let mut outcome = RcHttpHeaderOutcome {
        mode: McpProtocolMode::Legacy,
        protocol_version: None,
    };

    if let Some(value) = headers(RC_HEADER_PROTOCOL_VERSION) {
        if !is_supported_protocol_version(value) {
            return Err(unsupported_protocol_version_response(
                request_id.clone(),
                value,
            ));
        }
        outcome.protocol_version = Some(value.to_string());
        if value == DRAFT_PROTOCOL_VERSION {
            outcome.mode = McpProtocolMode::Modern;
        }
    }

    if let Some(method_header) = headers(RC_HEADER_METHOD) {
        outcome.mode = McpProtocolMode::Modern;
        if let Some(body_method) = body_method {
            if method_header != body_method {
                return Err(crate::jsonrpc::error_response_with_data(
                    request_id.clone(),
                    -32600,
                    "Mcp-Method header does not match request body",
                    json!({
                        "headerValue": method_header,
                        "bodyMethod": body_method,
                    }),
                ));
            }
        }
    }

    if let Some(name_header) = headers(RC_HEADER_NAME) {
        outcome.mode = McpProtocolMode::Modern;
        let expected = body_name.unwrap_or_default();
        if !expected.is_empty() && name_header != expected {
            return Err(crate::jsonrpc::error_response_with_data(
                request_id.clone(),
                -32600,
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

/// Pulls the standard `Mcp-Name` header value for a request body. RC
/// servers cross-check this against the header sent on the wire; RC
/// clients use the same helper when authoring outbound requests.
pub fn rc_name_header_value(method: &str, params: &JsonValue) -> Option<String> {
    match method {
        "tools/call" | "prompts/get" => params
            .get("name")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        "resources/read" => params
            .get("uri")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Modify a JSON-RPC result body in place to include the RC's
/// per-result discriminants when the response targets a Modern client.
/// Legacy clients see no change.
pub fn apply_rc_result_envelope(
    result: &mut JsonValue,
    mode: McpProtocolMode,
    cache: Option<&McpCacheHint>,
) {
    if !mode.is_modern() {
        return;
    }
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

/// Conservative cache hint surfaced on RC results. Servers can override
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

    /// Extract a cache hint from an RC result body. Returns `None` when
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

/// Build a JSON object mapping method names to their recorded RC cache
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
/// and any RC-required envelope fields.
pub fn server_discover_result(
    capabilities: JsonValue,
    server_info: JsonValue,
    instructions: Option<&str>,
) -> JsonValue {
    let mut result = json!({
        "resultType": RESULT_TYPE_COMPLETE,
        "protocolVersion": DRAFT_PROTOCOL_VERSION,
        "supportedVersions": supported_protocol_versions(),
        "capabilities": capabilities,
        "serverInfo": server_info,
    });
    if let Some(instructions) = instructions {
        result["instructions"] = JsonValue::String(instructions.to_string());
    }
    result
}

pub fn unsupported_client_bound_method_response(
    id: impl Into<JsonValue>,
    method: &str,
) -> Option<JsonValue> {
    let (feature, reason) = match method {
        METHOD_SAMPLING_CREATE_MESSAGE => (
            "sampling",
            "MCP sampling requests are server-to-client requests. Harn does not accept client-initiated sampling on MCP server endpoints.",
        ),
        METHOD_ELICITATION_CREATE => (
            "elicitation",
            "MCP elicitation requests are server-to-client requests. Harn MCP servers initiate elicitation from tool, resource, or prompt handlers instead of accepting it from clients.",
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
            "role": "client",
            "status": "unsupported",
            "reason": reason,
        }),
    ))
}

pub fn unsupported_task_augmentation_response(id: impl Into<JsonValue>, method: &str) -> JsonValue {
    task_augmentation_error_response(
        id,
        method,
        -32602,
        "MCP task-augmented execution is not supported",
        "Harn MCP tools execute inline and do not advertise taskSupport.",
    )
}

pub fn task_augmentation_error_response(
    id: impl Into<JsonValue>,
    method: &str,
    code: i64,
    message: &str,
    reason: &str,
) -> JsonValue {
    crate::jsonrpc::error_response_with_data(
        id,
        code,
        message,
        json!({
            "type": "mcp.unsupportedFeature",
            "protocolVersion": PROTOCOL_VERSION,
            "method": method,
            "feature": "tasks",
            "status": "unsupported",
            "reason": reason,
        }),
    )
}

pub fn requests_task_augmentation(params: &JsonValue) -> bool {
    params.get("task").is_some()
}

pub fn tasks_capability() -> JsonValue {
    json!({
        "list": {},
        "cancel": {},
        "requests": {
            "tools": {
                "call": {}
            }
        }
    })
}

pub fn completions_capability() -> JsonValue {
    json!({})
}

/// Severity levels defined by the MCP logging utility (RFC 5424 ordering).
///
/// Variants are ordered from most verbose (`Debug`) to most severe
/// (`Emergency`); `Ord` follows that ordering so that
/// `level >= subscribed_level` filters notifications by severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpLogLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

impl McpLogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Alert => "alert",
            Self::Emergency => "emergency",
        }
    }

    pub fn from_str_ci(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "notice" => Some(Self::Notice),
            "warning" | "warn" => Some(Self::Warning),
            "error" | "err" => Some(Self::Error),
            "critical" | "crit" => Some(Self::Critical),
            "alert" => Some(Self::Alert),
            "emergency" | "emerg" => Some(Self::Emergency),
            _ => None,
        }
    }
}

pub fn logging_capability() -> JsonValue {
    json!({})
}

/// Encode a `notifications/message` envelope per
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/utilities/logging>.
pub fn logging_message_notification(
    level: McpLogLevel,
    logger: Option<&str>,
    data: JsonValue,
) -> JsonValue {
    let mut params = serde_json::Map::new();
    params.insert(
        "level".to_string(),
        JsonValue::String(level.as_str().into()),
    );
    if let Some(logger) = logger {
        params.insert("logger".to_string(), JsonValue::String(logger.to_string()));
    }
    params.insert("data".to_string(), data);
    json!({
        "jsonrpc": "2.0",
        "method": METHOD_LOGGING_MESSAGE_NOTIFICATION,
        "params": JsonValue::Object(params),
    })
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

pub fn tool_execution(task_support: McpToolTaskSupport) -> JsonValue {
    json!({
        "taskSupport": task_support.as_str(),
    })
}

pub fn related_task_meta(task_id: &str) -> JsonValue {
    json!({
        RELATED_TASK_META_KEY: {
            "taskId": task_id,
        }
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
    fn task_augmentation_error_is_json_rpc_shaped() {
        let response = unsupported_task_augmentation_response(json!("call-1"), "tools/call");
        assert_eq!(response["jsonrpc"], json!("2.0"));
        assert_eq!(response["id"], json!("call-1"));
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
    }

    #[test]
    fn task_protocol_shapes_match_latest_spec_names() {
        assert_eq!(McpTaskStatus::Working.as_str(), "working");
        assert_eq!(McpTaskStatus::InputRequired.as_str(), "input_required");
        assert!(McpTaskStatus::Completed.is_terminal());
        assert_eq!(tasks_capability()["requests"]["tools"]["call"], json!({}));
        assert_eq!(
            tool_execution(McpToolTaskSupport::Optional)["taskSupport"],
            json!("optional")
        );
        assert_eq!(
            related_task_meta("task-1")[RELATED_TASK_META_KEY]["taskId"],
            json!("task-1")
        );
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
    fn log_levels_round_trip_through_string_form() {
        for level in [
            McpLogLevel::Debug,
            McpLogLevel::Info,
            McpLogLevel::Notice,
            McpLogLevel::Warning,
            McpLogLevel::Error,
            McpLogLevel::Critical,
            McpLogLevel::Alert,
            McpLogLevel::Emergency,
        ] {
            assert_eq!(McpLogLevel::from_str_ci(level.as_str()), Some(level));
        }
        assert_eq!(McpLogLevel::from_str_ci("WARN"), Some(McpLogLevel::Warning));
        assert_eq!(
            McpLogLevel::from_str_ci("Crit"),
            Some(McpLogLevel::Critical)
        );
        assert_eq!(McpLogLevel::from_str_ci(""), None);
        assert_eq!(McpLogLevel::from_str_ci("trace"), None);
    }

    #[test]
    fn log_levels_order_from_debug_to_emergency() {
        assert!(McpLogLevel::Debug < McpLogLevel::Info);
        assert!(McpLogLevel::Warning < McpLogLevel::Error);
        assert!(McpLogLevel::Error < McpLogLevel::Emergency);
    }

    #[test]
    fn logging_message_notification_matches_spec_envelope() {
        let notification = logging_message_notification(
            McpLogLevel::Warning,
            Some("audit.signature_verify"),
            json!({"event_id": 1, "kind": "verify_failed"}),
        );
        assert_eq!(notification["jsonrpc"], json!("2.0"));
        assert_eq!(
            notification["method"],
            json!(METHOD_LOGGING_MESSAGE_NOTIFICATION)
        );
        assert_eq!(notification["params"]["level"], json!("warning"));
        assert_eq!(
            notification["params"]["logger"],
            json!("audit.signature_verify")
        );
        assert_eq!(
            notification["params"]["data"]["kind"],
            json!("verify_failed")
        );

        let no_logger =
            logging_message_notification(McpLogLevel::Info, None, json!({"hello": "world"}));
        assert!(no_logger["params"].get("logger").is_none());
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
    fn rc_metadata_round_trips_through_meta_block() {
        let params = json!({
            "_meta": {
                RC_META_KEY_PROTOCOL_VERSION: DRAFT_PROTOCOL_VERSION,
                RC_META_KEY_CLIENT_INFO: {"name": "harn", "version": "x"},
                RC_META_KEY_CLIENT_CAPABILITIES: {"roots": {}},
            }
        });
        let meta = parse_request_metadata(&params);
        assert_eq!(
            meta.protocol_version.as_deref(),
            Some(DRAFT_PROTOCOL_VERSION)
        );
        assert_eq!(
            meta.client_info,
            Some(json!({"name": "harn", "version": "x"}))
        );
        assert_eq!(meta.client_capabilities, Some(json!({"roots": {}})));
        assert_eq!(meta.mode(), McpProtocolMode::Modern);
    }

    #[test]
    fn rc_metadata_defaults_to_legacy_when_absent() {
        let meta = parse_request_metadata(&json!({}));
        assert_eq!(meta, McpRequestMetadata::default());
        assert_eq!(meta.mode(), McpProtocolMode::Legacy);
    }

    #[test]
    fn enforce_request_protocol_version_rejects_unknown_version() {
        let meta = McpRequestMetadata {
            protocol_version: Some("2099-01-01".to_string()),
            ..Default::default()
        };
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
        assert!(supported.iter().any(|v| v == DRAFT_PROTOCOL_VERSION));
        assert!(supported.iter().any(|v| v == PROTOCOL_VERSION));
        assert!(supported
            .iter()
            .any(|v| v == LEGACY_2025_06_18_PROTOCOL_VERSION));
    }

    #[test]
    fn enforce_request_protocol_version_returns_modern_mode_for_draft() {
        let meta = McpRequestMetadata {
            protocol_version: Some(DRAFT_PROTOCOL_VERSION.to_string()),
            ..Default::default()
        };
        let mode = enforce_request_protocol_version(&json!(1), &meta).unwrap();
        assert_eq!(mode, Some(McpProtocolMode::Modern));
    }

    #[test]
    fn enforce_request_protocol_version_accepts_2025_06_18_as_legacy() {
        let meta = McpRequestMetadata {
            protocol_version: Some(LEGACY_2025_06_18_PROTOCOL_VERSION.to_string()),
            ..Default::default()
        };
        let mode = enforce_request_protocol_version(&json!(1), &meta).unwrap();
        assert_eq!(mode, Some(McpProtocolMode::Legacy));
    }

    #[test]
    fn negotiate_rc_http_headers_detects_draft_protocol_header() {
        let headers = std::collections::HashMap::from([(
            RC_HEADER_PROTOCOL_VERSION.to_string(),
            DRAFT_PROTOCOL_VERSION.to_string(),
        )]);
        let outcome = negotiate_rc_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/list"),
            None,
            &json!(1),
        )
        .unwrap();
        assert_eq!(outcome.mode, McpProtocolMode::Modern);
        assert_eq!(
            outcome.protocol_version.as_deref(),
            Some(DRAFT_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn negotiate_rc_http_headers_rejects_method_body_mismatch() {
        let headers = std::collections::HashMap::from([(
            RC_HEADER_METHOD.to_string(),
            "tools/list".to_string(),
        )]);
        let err = negotiate_rc_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/call"),
            None,
            &json!(2),
        )
        .expect_err("header/body mismatch must error");
        assert_eq!(err["error"]["code"], json!(-32600));
        assert_eq!(err["error"]["data"]["headerValue"], json!("tools/list"));
        assert_eq!(err["error"]["data"]["bodyMethod"], json!("tools/call"));
    }

    #[test]
    fn negotiate_rc_http_headers_rejects_name_body_mismatch() {
        let headers = std::collections::HashMap::from([
            (RC_HEADER_METHOD.to_string(), "tools/call".to_string()),
            (RC_HEADER_NAME.to_string(), "wrong".to_string()),
        ]);
        let err = negotiate_rc_http_request(
            |key| headers.get(key).map(String::as_str),
            Some("tools/call"),
            Some("right"),
            &json!(3),
        )
        .expect_err("name mismatch must error");
        assert_eq!(err["error"]["code"], json!(-32600));
        assert_eq!(err["error"]["data"]["bodyName"], json!("right"));
    }

    #[test]
    fn rc_name_header_value_extracts_method_subject() {
        assert_eq!(
            rc_name_header_value("tools/call", &json!({"name": "demo"})),
            Some("demo".to_string())
        );
        assert_eq!(
            rc_name_header_value("prompts/get", &json!({"name": "p"})),
            Some("p".to_string())
        );
        assert_eq!(
            rc_name_header_value("resources/read", &json!({"uri": "harn://x"})),
            Some("harn://x".to_string())
        );
        assert_eq!(rc_name_header_value("tools/list", &json!({})), None);
    }

    #[test]
    fn apply_rc_result_envelope_adds_result_type_and_cache_only_for_modern() {
        let mut modern = json!({"tools": []});
        apply_rc_result_envelope(
            &mut modern,
            McpProtocolMode::Modern,
            Some(&McpCacheHint::list_default()),
        );
        assert_eq!(modern["resultType"], json!(RESULT_TYPE_COMPLETE));
        assert_eq!(modern["ttlMs"], json!(DEFAULT_LIST_CACHE_TTL_MS));
        assert_eq!(modern["cacheScope"], json!(DEFAULT_LIST_CACHE_SCOPE));

        let mut legacy = json!({"tools": []});
        apply_rc_result_envelope(
            &mut legacy,
            McpProtocolMode::Legacy,
            Some(&McpCacheHint::list_default()),
        );
        assert!(legacy.get("resultType").is_none());
        assert!(legacy.get("ttlMs").is_none());
        assert!(legacy.get("cacheScope").is_none());
    }

    #[test]
    fn apply_rc_result_envelope_preserves_caller_provided_result_type() {
        let mut result = json!({"resultType": RESULT_TYPE_INPUT_REQUIRED});
        apply_rc_result_envelope(&mut result, McpProtocolMode::Modern, None);
        assert_eq!(result["resultType"], json!(RESULT_TYPE_INPUT_REQUIRED));
    }

    #[test]
    fn server_discover_result_advertises_both_versions() {
        let discover = server_discover_result(
            json!({"tools": {}}),
            json!({"name": "harn", "version": "x"}),
            Some("hello"),
        );
        assert_eq!(discover["resultType"], json!(RESULT_TYPE_COMPLETE));
        assert_eq!(discover["protocolVersion"], json!(DRAFT_PROTOCOL_VERSION));
        let supported = discover["supportedVersions"].as_array().unwrap();
        assert!(supported.iter().any(|v| v == DRAFT_PROTOCOL_VERSION));
        assert!(supported.iter().any(|v| v == PROTOCOL_VERSION));
        assert!(supported
            .iter()
            .any(|v| v == LEGACY_2025_06_18_PROTOCOL_VERSION));
        assert_eq!(discover["instructions"], json!("hello"));
    }
}
