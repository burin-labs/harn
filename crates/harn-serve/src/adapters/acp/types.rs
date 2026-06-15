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
pub const ACP_METHOD_SESSION_REPLACE_INJECT: &str = "session/replace_inject";
pub const ACP_METHOD_SESSION_REVOKE_INJECT: &str = "session/revoke_inject";
pub const ACP_METHOD_SESSION_PENDING_INJECTIONS: &str = "session/pending_injections";

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

/// Response envelope for failed ACP JSON-RPC calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpJsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: AcpJsonRpcId,
    pub error: AcpJsonRpcError,
}

/// `session/new` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionNewParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AcpSessionNewParams {
    pub fn cwd(cwd: impl Into<String>) -> Self {
        Self {
            cwd: Some(cwd.into()),
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
}
