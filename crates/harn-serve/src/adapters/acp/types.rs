//! Public ACP wire helpers for Rust embedders.
//!
//! These types cover the stable request shapes embedders most often send to
//! `harn serve acp` or [`crate::EmbeddedAgent`]. They intentionally preserve
//! ACP's JSON field names so callers can serialize them directly onto the wire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const ACP_METHOD_INITIALIZE: &str = "initialize";
pub const ACP_METHOD_SESSION_NEW: &str = "session/new";
pub const ACP_METHOD_SESSION_LOAD: &str = "session/load";
pub const ACP_METHOD_SESSION_RESUME: &str = "session/resume";
pub const ACP_METHOD_SESSION_PROMPT: &str = "session/prompt";
pub const ACP_METHOD_SESSION_CANCEL: &str = "session/cancel";
pub const ACP_METHOD_SESSION_CANCEL_TOOL_CALL: &str = "session/cancel_tool_call";
pub const ACP_METHOD_SESSION_CLOSE: &str = "session/close";
pub const ACP_METHOD_SESSION_INJECT: &str = "session/inject";
pub const ACP_METHOD_SESSION_INJECT_HOST_EVENT: &str = "session/inject_host_event";
pub const ACP_METHOD_SESSION_REPLACE_INJECT: &str = "session/replace_inject";
pub const ACP_METHOD_SESSION_REVOKE_INJECT: &str = "session/revoke_inject";
pub const ACP_METHOD_SESSION_PENDING_INJECTIONS: &str = "session/pending_injections";
pub const ACP_PROMPT_ERROR_DATA_SCHEMA: &str = "harn.acp.prompt_error.v1";

/// JSON-RPC id values accepted by ACP requests and responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum AcpJsonRpcId {
    Number(u64),
    String(String),
    Null,
}

impl From<u64> for AcpJsonRpcId {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

impl From<&str> for AcpJsonRpcId {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<String> for AcpJsonRpcId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

/// A typed JSON-RPC request envelope for ACP methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpJsonRpcRequest<P = serde_json::Value> {
    pub jsonrpc: String,
    pub id: AcpJsonRpcId,
    pub method: String,
    pub params: P,
}

impl<P> AcpJsonRpcRequest<P> {
    pub fn new(id: impl Into<AcpJsonRpcId>, method: impl Into<String>, params: P) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

impl<P: Serialize> AcpJsonRpcRequest<P> {
    /// Serialize this request into the `serde_json::Value` expected by the
    /// in-process ACP channel transport.
    pub fn into_json_value(self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Serialize this request as one JSON-RPC line for stdio or WebSocket
    /// text-frame transports.
    pub fn into_json_line(self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self)
    }
}

impl AcpJsonRpcRequest<serde_json::Value> {
    pub fn initialize(id: impl Into<AcpJsonRpcId>) -> Self {
        Self::new(id, ACP_METHOD_INITIALIZE, serde_json::json!({}))
    }
}

impl AcpJsonRpcRequest<AcpSessionNewParams> {
    pub fn session_new(id: impl Into<AcpJsonRpcId>, params: AcpSessionNewParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_NEW, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionPromptParams> {
    pub fn session_prompt(id: impl Into<AcpJsonRpcId>, params: AcpSessionPromptParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_PROMPT, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionIdParams> {
    pub fn session_load(id: impl Into<AcpJsonRpcId>, params: AcpSessionIdParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_LOAD, params)
    }

    pub fn session_resume(id: impl Into<AcpJsonRpcId>, params: AcpSessionIdParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_RESUME, params)
    }

    pub fn session_cancel(id: impl Into<AcpJsonRpcId>, params: AcpSessionIdParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_CANCEL, params)
    }

    pub fn session_close(id: impl Into<AcpJsonRpcId>, params: AcpSessionIdParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_CLOSE, params)
    }

    pub fn session_pending_injections(
        id: impl Into<AcpJsonRpcId>,
        params: AcpSessionIdParams,
    ) -> Self {
        Self::new(id, ACP_METHOD_SESSION_PENDING_INJECTIONS, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionInjectParams> {
    pub fn session_inject(id: impl Into<AcpJsonRpcId>, params: AcpSessionInjectParams) -> Self {
        Self::new(id, ACP_METHOD_SESSION_INJECT, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionInjectHostEventParams> {
    pub fn session_inject_host_event(
        id: impl Into<AcpJsonRpcId>,
        params: AcpSessionInjectHostEventParams,
    ) -> Self {
        Self::new(id, ACP_METHOD_SESSION_INJECT_HOST_EVENT, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionReplaceInjectParams> {
    pub fn session_replace_inject(
        id: impl Into<AcpJsonRpcId>,
        params: AcpSessionReplaceInjectParams,
    ) -> Self {
        Self::new(id, ACP_METHOD_SESSION_REPLACE_INJECT, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionMessageIdParams> {
    pub fn session_revoke_inject(
        id: impl Into<AcpJsonRpcId>,
        params: AcpSessionMessageIdParams,
    ) -> Self {
        Self::new(id, ACP_METHOD_SESSION_REVOKE_INJECT, params)
    }
}

impl AcpJsonRpcRequest<AcpSessionCancelToolCallParams> {
    pub fn session_cancel_tool_call(
        id: impl Into<AcpJsonRpcId>,
        params: AcpSessionCancelToolCallParams,
    ) -> Self {
        Self::new(id, ACP_METHOD_SESSION_CANCEL_TOOL_CALL, params)
    }
}

/// Response envelope for successful ACP JSON-RPC calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpJsonRpcResponse<R = serde_json::Value> {
    pub jsonrpc: String,
    pub id: AcpJsonRpcId,
    pub result: R,
}

/// Common JSON-RPC error payload returned by ACP.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpJsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Harn-owned machine data attached to a failed `session/prompt` response.
///
/// `message` remains lossless human diagnostics on the JSON-RPC error itself;
/// hosts branch only on this stable class and never reconstruct it from prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpPromptErrorSchema {
    #[serde(rename = "harn.acp.prompt_error.v1")]
    V1,
}

/// Machine-branchable facts projected from a terminal prompt failure.
///
/// Every field is optional and omitted when unknown, so the payload stays
/// additive on the `harn.acp.prompt_error.v1` envelope: a host that only reads
/// `schema` + `terminalClass` keeps working, while a routing-aware host reads
/// the authoritative provider/model of the route that actually failed instead
/// of inferring it from the session's UI model selection or parsing prose.
///
/// A non-provider failure (compile, setup, protocol) carries the same shape
/// with `provider`/`model` absent — the absence is itself the signal that no
/// route is responsible. These names mirror the structured error dict
/// `llm_call` throws (`category`/`kind`/`reason`/`code`/`retryAfterMs`/
/// `provider`/`model`), so the projection never invents a parallel vocabulary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPromptFailureFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The per-route ledger the routing failure recorded: which provider/model
    /// each attempt used and how it ended. Empty for a single-route call or any
    /// non-routing failure. Lets a host render the full failover chain instead
    /// of just the terminal route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<AcpRoutingAttempt>,
    /// Set when no single route is responsible for the terminal outcome (e.g.
    /// both racers hit the deadline). It is the machine signal that the absence
    /// of `provider`/`model` is authoritative, not merely unknown — a host must
    /// not infer a route from its own model selection.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub route_unknown: bool,
}

/// One route the routing failure tried, projected from the thrown error's
/// `attempts` ledger. Only the machine-branchable identity/outcome fields are
/// carried (no floating-point cost) so the facts stay `Eq`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpRoutingAttempt {
    pub index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl AcpPromptFailureFacts {
    /// Project the structured error dict `llm_call`/routing throws into the
    /// stable failure facts. Non-object input (a bare thrown string, or a
    /// compile/setup error) yields empty facts, so provider/model are absent
    /// rather than fabricated.
    pub fn from_thrown(thrown: &serde_json::Value) -> Self {
        let Some(object) = thrown.as_object() else {
            return Self::default();
        };
        let string_field = |key: &str| {
            object
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        let kind = string_field("kind");
        let retry_after_ms = object
            .get("retry_after_ms")
            .and_then(serde_json::Value::as_i64);
        // `retryable` is only asserted when the producer gave us a signal for
        // it: an explicit `transient`/`terminal` kind, or a `retry-after` hint.
        // Otherwise it stays absent — we do not guess a boolean.
        let retryable = match kind.as_deref() {
            Some("transient") => Some(true),
            Some("terminal") => Some(false),
            _ => retry_after_ms.map(|_| true),
        };
        // `no_single_route` is the routing layer's no-fabrication signal (both
        // racers hit the deadline). Project it as `routeUnknown` so a host reads
        // the absent provider/model as authoritative, not merely missing.
        let route_unknown = object
            .get("no_single_route")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let attempts = object
            .get("attempts")
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().map(AcpRoutingAttempt::from_value).collect())
            .unwrap_or_default();
        Self {
            category: string_field("category"),
            kind,
            reason: string_field("reason"),
            code: string_field("code"),
            retryable,
            retry_after_ms,
            provider: string_field("provider"),
            model: string_field("model"),
            attempts,
            route_unknown,
        }
    }
}

impl AcpRoutingAttempt {
    /// Project one entry of the thrown `attempts` ledger. The nested `error`
    /// object (when present) carries the per-route category/reason.
    fn from_value(value: &serde_json::Value) -> Self {
        let string_field = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let error = value.get("error");
        let error_field = |key: &str| {
            error
                .and_then(|err| err.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        Self {
            index: value
                .get("index")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            provider: string_field("provider"),
            model: string_field("model"),
            status: string_field("status"),
            category: error_field("category"),
            reason: error_field("reason"),
        }
    }
}

/// Harn-owned typed `error.data` for a failed `session/prompt` response.
///
/// One terminal failure produces exactly one JSON-RPC error carrying this
/// payload and never an assistant `agent_message_chunk`; `message` remains
/// lossless human diagnostics on the JSON-RPC error itself, while this data is
/// the stable machine contract hosts branch on. Fields beyond `terminalClass`
/// are flattened onto the envelope and additive, so the complementary
/// success-path terminal outcome (harn#4834) can later reuse the same
/// `terminalClass`/`reason` spine on the success frame without another breaking
/// change to this shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPromptErrorData {
    pub schema: AcpPromptErrorSchema,
    pub terminal_class: harn_vm::llm::AgentTerminalClass,
    #[serde(flatten)]
    pub facts: AcpPromptFailureFacts,
}

impl AcpPromptErrorData {
    pub fn new(terminal_class: harn_vm::llm::AgentTerminalClass) -> Self {
        Self::with_facts(terminal_class, AcpPromptFailureFacts::default())
    }

    pub fn with_facts(
        terminal_class: harn_vm::llm::AgentTerminalClass,
        facts: AcpPromptFailureFacts,
    ) -> Self {
        Self {
            schema: AcpPromptErrorSchema::V1,
            terminal_class,
            facts,
        }
    }
}

/// Response envelope for failed ACP JSON-RPC calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpJsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: AcpJsonRpcId,
    pub error: AcpJsonRpcError,
}

/// The capability profile a client declares on `session/new`. Optional: absent,
/// the session runs the legacy no-profile path (subprocesses inherit the server
/// environment). Present, harn resolves it into a
/// [`harn_vm::security::SessionProfile`] at launch and every prompt turn's
/// subprocesses run under the closed allowlist + grants environment.
///
/// The launcher parses its own `--grant name=spec` strings at ITS boundary and
/// sends harn this already-typed, value-free shape; harn does not parse flag
/// strings. A hermetic kind with any grant is rejected at launch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionProfileConfig {
    pub kind: harn_vm::security::SessionProfileKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<harn_vm::security::GrantSpec>,
}

/// `session/new` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionNewParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The session's declared capability profile, if any. See
    /// [`AcpSessionProfileConfig`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AcpSessionProfileConfig>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AcpSessionNewParams {
    pub fn cwd(cwd: impl Into<String>) -> Self {
        Self {
            cwd: Some(cwd.into()),
            profile: None,
            extra: BTreeMap::new(),
        }
    }
}

/// `session/new`, `session/load`, and `session/resume` result fields Harn
/// returns for an active ACP session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionRestoreResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<serde_json::Value>,
    #[serde(
        rename = "configOptions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub config_options: Option<serde_json::Value>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Params containing only an ACP session id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionIdParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

impl AcpSessionIdParams {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

/// `session/prompt` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionPromptParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub prompt: Vec<AcpContentBlock>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AcpSessionPromptParams {
    pub fn new(session_id: impl Into<String>, prompt: Vec<AcpContentBlock>) -> Self {
        Self {
            session_id: session_id.into(),
            prompt,
            extra: BTreeMap::new(),
        }
    }

    pub fn text(session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(session_id, vec![AcpContentBlock::text(text)])
    }
}

/// `session/prompt` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionPromptResult {
    #[serde(rename = "stopReason")]
    pub stop_reason: String,
}

/// ACP content blocks accepted by Harn prompt and injection requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AcpContentBlock {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mimeType", alias = "media_type")]
        mime_type: String,
        #[serde(default, alias = "base64", skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(
            default,
            alias = "url",
            alias = "source_uri",
            skip_serializing_if = "Option::is_none"
        )]
        uri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Audio {
        #[serde(rename = "mimeType", alias = "media_type")]
        mime_type: String,
        #[serde(default, alias = "base64", skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(
            default,
            alias = "url",
            alias = "source_uri",
            skip_serializing_if = "Option::is_none"
        )]
        uri: Option<String>,
    },
    Resource {
        resource: AcpEmbeddedResource,
    },
    ResourceLink {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(
            rename = "mimeType",
            alias = "media_type",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
}

impl AcpContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn image_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            mime_type: mime_type.into(),
            data: Some(data.into()),
            uri: None,
            detail: None,
        }
    }

    pub fn image_uri(mime_type: impl Into<String>, uri: impl Into<String>) -> Self {
        Self::Image {
            mime_type: mime_type.into(),
            data: None,
            uri: Some(uri.into()),
            detail: None,
        }
    }

    pub fn audio_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Audio {
            mime_type: mime_type.into(),
            data: Some(data.into()),
            uri: None,
        }
    }

    pub fn audio_uri(mime_type: impl Into<String>, uri: impl Into<String>) -> Self {
        Self::Audio {
            mime_type: mime_type.into(),
            data: None,
            uri: Some(uri.into()),
        }
    }

    pub fn embedded_text_resource(
        uri: impl Into<String>,
        mime_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::Resource {
            resource: AcpEmbeddedResource {
                uri: uri.into(),
                mime_type: Some(mime_type.into()),
                text: Some(text.into()),
                blob: None,
            },
        }
    }

    pub fn resource_link(uri: impl Into<String>) -> Self {
        Self::ResourceLink {
            uri: uri.into(),
            name: None,
            title: None,
            description: None,
            mime_type: None,
            size: None,
        }
    }
}

/// Embedded resource payload nested under an ACP `resource` content block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpEmbeddedResource {
    pub uri: String,
    #[serde(rename = "mimeType", alias = "media_type")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// Delivery mode for `session/inject` queued user messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AcpSessionInjectMode {
    Queue,
    Steer,
}

/// `session/inject` content accepts either a plain string or ACP content blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum AcpSessionInjectContent {
    Text(String),
    Blocks(Vec<AcpContentBlock>),
}

impl From<String> for AcpSessionInjectContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for AcpSessionInjectContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<AcpContentBlock>> for AcpSessionInjectContent {
    fn from(value: Vec<AcpContentBlock>) -> Self {
        Self::Blocks(value)
    }
}

/// Standard ACP `_meta` wrapper for Harn-owned extension fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpMeta {
    pub harn: AcpHarnMeta,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AcpMeta {
    pub fn actor(actor: serde_json::Value) -> Self {
        Self {
            harn: AcpHarnMeta {
                actor: Some(actor),
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        }
    }
}

/// Harn-owned ACP `_meta.harn` extension fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpHarnMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<serde_json::Value>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `session/inject` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionInjectParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub mode: AcpSessionInjectMode,
    pub content: AcpSessionInjectContent,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<AcpMeta>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Harn ACP extension params for injecting a typed, provenance-bearing host event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpSessionInjectHostEventParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub event: harn_vm::agent_sessions::HostInjectionRequest,
}

impl AcpSessionInjectHostEventParams {
    pub fn new(
        session_id: impl Into<String>,
        event: harn_vm::agent_sessions::HostInjectionRequest,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            event,
        }
    }
}

impl AcpSessionInjectParams {
    pub fn new(
        session_id: impl Into<String>,
        mode: AcpSessionInjectMode,
        content: impl Into<AcpSessionInjectContent>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            mode,
            content: content.into(),
            meta: None,
            extra: BTreeMap::new(),
        }
    }
}

/// `session/replace_inject` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionReplaceInjectParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub content: AcpSessionInjectContent,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<AcpMeta>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AcpSessionReplaceInjectParams {
    pub fn new(
        session_id: impl Into<String>,
        message_id: impl Into<String>,
        content: impl Into<AcpSessionInjectContent>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            message_id: message_id.into(),
            content: content.into(),
            meta: None,
            extra: BTreeMap::new(),
        }
    }
}

/// Params for pending-message operations such as `session/revoke_inject`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionMessageIdParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
}

impl AcpSessionMessageIdParams {
    pub fn new(session_id: impl Into<String>, message_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            message_id: message_id.into(),
        }
    }
}

/// `session/cancel_tool_call` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionCancelToolCallParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(
        rename = "injectReminder",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub inject_reminder: Option<bool>,
}

impl AcpSessionCancelToolCallParams {
    pub fn new(session_id: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            tool_call_id: tool_call_id.into(),
            reason: None,
            inject_reminder: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_prompt_request_serializes_to_acp_wire_shape() {
        let value =
            AcpJsonRpcRequest::session_prompt(7, AcpSessionPromptParams::text("sess-1", "hello"))
                .into_json_value()
                .expect("request serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "session/prompt",
                "params": {
                    "sessionId": "sess-1",
                    "prompt": [{"type": "text", "text": "hello"}],
                },
            })
        );
    }

    #[test]
    fn session_inject_request_serializes_mode_and_content() {
        let value = AcpJsonRpcRequest::session_inject(
            "inject-1",
            AcpSessionInjectParams::new(
                "sess-1",
                AcpSessionInjectMode::Steer,
                vec![AcpContentBlock::text("interrupt after this step")],
            ),
        )
        .into_json_value()
        .expect("request serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "inject-1",
                "method": "session/inject",
                "params": {
                    "sessionId": "sess-1",
                    "mode": "steer",
                    "content": [{"type": "text", "text": "interrupt after this step"}],
                },
            })
        );
    }

    #[test]
    fn resource_link_content_serializes_to_acp_wire_shape() {
        let value = serde_json::to_value(AcpContentBlock::resource_link("file:///tmp/report.md"))
            .expect("resource link block serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "type": "resource_link",
                "uri": "file:///tmp/report.md",
            })
        );
    }

    #[test]
    fn prompt_failure_facts_project_the_routed_provider_and_model() {
        let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!({
            "category": "generic",
            "kind": "transient",
            "reason": "rate_limit",
            "code": "provider_exhausted",
            "message": "429 from the backup route",
            "retry_after_ms": 1200,
            // The route that actually failed after a ladder advance — distinct
            // from any base/requested route the session selected.
            "provider": "backup-provider",
            "model": "escalated-model",
        }));

        assert_eq!(facts.category.as_deref(), Some("generic"));
        assert_eq!(facts.kind.as_deref(), Some("transient"));
        assert_eq!(facts.reason.as_deref(), Some("rate_limit"));
        assert_eq!(facts.code.as_deref(), Some("provider_exhausted"));
        assert_eq!(facts.retryable, Some(true));
        assert_eq!(facts.retry_after_ms, Some(1200));
        assert_eq!(facts.provider.as_deref(), Some("backup-provider"));
        assert_eq!(facts.model.as_deref(), Some("escalated-model"));
    }

    #[test]
    fn prompt_failure_facts_are_empty_for_non_object_throws() {
        let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!("bare string throw"));
        assert_eq!(facts, AcpPromptFailureFacts::default());
    }

    #[test]
    fn terminal_kind_marks_failure_not_retryable() {
        let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!({
            "kind": "terminal",
            "reason": "provider_exhausted",
        }));
        assert_eq!(facts.retryable, Some(false));
    }

    #[test]
    fn prompt_failure_facts_project_the_routing_attempt_ledger() {
        // A provider-exhausted routing failure carries the per-route ledger plus
        // the authoritative terminal provider/model. The facts project both, so
        // a host can render the full failover chain.
        let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!({
            "kind": "terminal",
            "reason": "provider_exhausted",
            "provider": "backup-provider",
            "model": "backup-model",
            "attempts": [
                {
                    "index": 1,
                    "provider": "primary-provider",
                    "model": "primary-model",
                    "status": "failed",
                    "error": {"category": "circuit_open", "reason": "overloaded"},
                },
                {
                    "index": 2,
                    "provider": "backup-provider",
                    "model": "backup-model",
                    "status": "failed",
                    "error": {"category": "timeout", "reason": "deadline"},
                },
            ],
        }));

        assert_eq!(facts.provider.as_deref(), Some("backup-provider"));
        assert!(!facts.route_unknown);
        assert_eq!(facts.attempts.len(), 2);
        assert_eq!(facts.attempts[0].index, 1);
        assert_eq!(
            facts.attempts[0].provider.as_deref(),
            Some("primary-provider")
        );
        assert_eq!(facts.attempts[0].status.as_deref(), Some("failed"));
        assert_eq!(facts.attempts[0].category.as_deref(), Some("circuit_open"));
        assert_eq!(facts.attempts[1].model.as_deref(), Some("backup-model"));
        assert_eq!(facts.attempts[1].reason.as_deref(), Some("deadline"));
    }

    #[test]
    fn composite_failure_projects_route_unknown_with_no_provider() {
        // No single route is responsible: `no_single_route` becomes `routeUnknown`
        // and no provider/model is present or fabricated.
        let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!({
            "kind": "terminal",
            "reason": "provider_exhausted",
            "no_single_route": true,
            "attempts": [
                {"index": 1, "provider": "primary-provider", "model": "primary-model", "status": "failed"},
                {"index": 2, "provider": "backup-provider", "model": "backup-model", "status": "failed"},
            ],
        }));

        assert!(facts.route_unknown, "composite failure sets routeUnknown");
        assert!(
            facts.provider.is_none(),
            "composite must not carry a provider"
        );
        assert!(facts.model.is_none(), "composite must not carry a model");
        assert_eq!(facts.attempts.len(), 2);
    }

    #[test]
    fn route_unknown_and_attempts_are_omitted_when_absent() {
        // The additive fields must not appear on a plain single-route failure,
        // keeping the envelope byte-identical for existing hosts.
        let facts = AcpPromptFailureFacts::from_thrown(&serde_json::json!({
            "kind": "terminal",
            "reason": "timeout",
            "provider": "acme",
            "model": "acme-large",
        }));
        assert!(facts.attempts.is_empty());
        assert!(!facts.route_unknown);

        let wire = serde_json::to_value(&facts).expect("serialize");
        let object = wire.as_object().expect("facts serialize to an object");
        assert!(!object.contains_key("attempts"));
        assert!(!object.contains_key("routeUnknown"));
    }

    #[test]
    fn prompt_error_data_round_trips_through_the_flattened_envelope() {
        let data = AcpPromptErrorData::with_facts(
            harn_vm::llm::AgentTerminalClass::RateLimited,
            AcpPromptFailureFacts::from_thrown(&serde_json::json!({
                "kind": "transient",
                "reason": "rate_limit",
                "provider": "acme",
                "model": "acme-large",
            })),
        );

        let wire = serde_json::to_value(&data).expect("serialize");
        assert_eq!(
            wire,
            serde_json::json!({
                "schema": ACP_PROMPT_ERROR_DATA_SCHEMA,
                "terminalClass": "rate_limited",
                "kind": "transient",
                "reason": "rate_limit",
                "retryable": true,
                "provider": "acme",
                "model": "acme-large",
            })
        );

        let restored: AcpPromptErrorData = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(restored, data);
    }

    #[test]
    fn minimal_prompt_error_data_omits_absent_facts() {
        let wire = serde_json::to_value(AcpPromptErrorData::new(
            harn_vm::llm::AgentTerminalClass::GenericThrow,
        ))
        .expect("serialize");

        // A bare failure stays byte-for-byte compatible with the pre-enrichment
        // `{schema, terminalClass}` shape so v1 consumers keep parsing.
        assert_eq!(
            wire,
            serde_json::json!({
                "schema": ACP_PROMPT_ERROR_DATA_SCHEMA,
                "terminalClass": "generic_throw",
            })
        );
    }
}
