//! MCP `elicitation/create` plumbing — server-to-client structured prompts.
//!
//! When Harn is acting as an MCP **server**, a tool handler can call
//! `mcp_elicit({ message, requestedSchema })` to ask the connected
//! client to surface a structured prompt to its end user. The reply
//! envelope is `{ action: "accept" | "decline" | "cancel", content?: ... }`,
//! where `content` is validated against `requestedSchema` (a JSON Schema
//! restricted to a flat object of primitives per MCP 2025-11-25).
//!
//! When Harn is acting as an MCP **client**, an inbound
//! `elicitation/create` request from a peer server is dispatched to the
//! embedder via the `HostCallBridge` (`capability="mcp"`,
//! `operation="elicit"`). If no host bridge is wired up, the client
//! responds with `{ action: "decline" }` so the server can make a
//! sensible fallback decision.
//!
//! See the spec at
//! <https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation>.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as JsonValue};
use tokio::sync::{mpsc, oneshot};

use crate::schema::{elicitation_validate, elicitation_validate_schema, json_to_vm_value};
use crate::stdlib::host::{dispatch_host_call_bridge, dispatch_mock_host_call};
use crate::value::{VmError, VmValue};

/// JSON-RPC method name for elicitation requests.
pub const ELICITATION_METHOD: &str = "elicitation/create";

/// Outbound message sink — typically wraps the MCP transport's writer
/// (stdout for stdio servers, an SSE channel for HTTP servers).
pub type OutboundSender = mpsc::UnboundedSender<JsonValue>;

/// Per-connection bus that owns in-flight `elicitation/create` requests.
///
/// Cheap to clone (every clone shares the same pending map), so the
/// transport layer can hand a copy to its reader task while the dispatch
/// loop installs the same bus thread-locally for tool handlers.
#[derive(Clone)]
pub struct ElicitationBus {
    outbound: OutboundSender,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<JsonValue>>>>,
    next_id: Arc<AtomicU64>,
}

impl ElicitationBus {
    pub fn new(outbound: OutboundSender) -> Self {
        Self {
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Try to dispatch `msg` as a response to a pending elicitation
    /// request. Returns `true` when the message was a JSON-RPC response
    /// whose id matches an in-flight elicitation (and the bus delivered
    /// it). Otherwise the caller should treat `msg` as a new inbound
    /// request.
    pub fn route_response(&self, msg: &JsonValue) -> bool {
        // Responses have an id and either `result` or `error`, but no
        // `method`. Notifications have a method but no id. Be strict so
        // we don't accidentally swallow client-initiated requests that
        // happen to share a string id with a recent elicitation.
        if msg.get("method").is_some() {
            return false;
        }
        if msg.get("result").is_none() && msg.get("error").is_none() {
            return false;
        }
        let Some(id) = msg.get("id") else {
            return false;
        };
        let id_key = canonical_id(id);
        let mut pending = self.pending.lock().expect("elicitation pending poisoned");
        if let Some(tx) = pending.remove(&id_key) {
            // The receiver may have been dropped (e.g. the awaiting tool
            // handler timed out). Sending into a closed oneshot is fine
            // — we still ate the response so the dispatcher won't try
            // to handle it as a request.
            let _ = tx.send(msg.clone());
            true
        } else {
            false
        }
    }

    /// Send an `elicitation/create` request to the peer and await its
    /// reply. The returned envelope follows the spec: `{ action, content? }`.
    /// `content` is validated against `requested_schema` when present
    /// and the action is `accept`.
    pub async fn elicit(
        &self,
        message: String,
        requested_schema: JsonValue,
    ) -> Result<VmValue, VmError> {
        validate_requested_schema(&requested_schema)?;

        let id_seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("harn-elicit-{id_seq}");
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("elicitation pending poisoned")
            .insert(id.clone(), tx);

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": ELICITATION_METHOD,
            "params": {
                "message": message,
                "requestedSchema": requested_schema,
            },
        });

        if self.outbound.send(request).is_err() {
            self.pending
                .lock()
                .expect("elicitation pending poisoned")
                .remove(&id);
            return Err(VmError::Runtime(
                "mcp_elicit: transport closed before request could be sent".into(),
            ));
        }

        let response = match rx.await {
            Ok(value) => value,
            Err(_) => {
                // Receiver dropped before a response arrived — most
                // likely the transport closed mid-flight.
                return Err(VmError::Runtime(
                    "mcp_elicit: transport dropped before client responded".into(),
                ));
            }
        };

        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error");
            let code = error
                .get("code")
                .and_then(|value| value.as_i64())
                .unwrap_or(-1);
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "mcp_elicit: client error ({code}): {message}"
            )))));
        }

        let result = response.get("result").cloned().unwrap_or(JsonValue::Null);
        envelope_from_response(&result, &requested_schema)
    }
}

/// Coerce JSON-RPC ids (which may be strings or numbers per spec) into
/// a single string key for the pending-response map.
fn canonical_id(value: &JsonValue) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(n) = value.as_i64() {
        return n.to_string();
    }
    if let Some(n) = value.as_u64() {
        return n.to_string();
    }
    value.to_string()
}

/// Spec-compliant elicitation request schemas are flat objects whose
/// properties are primitive types (string / number / integer / boolean),
/// optionally with `enum` or numeric/length bounds. We don't enforce the
/// full restriction, but we do require an object schema so that
/// validation has well-defined semantics.
fn validate_requested_schema(schema: &JsonValue) -> Result<(), VmError> {
    let object = schema.as_object().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "mcp_elicit: requestedSchema must be a JSON object",
        )))
    })?;
    match object.get("type").and_then(|value| value.as_str()) {
        Some("object") => Ok(()),
        Some(other) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "mcp_elicit: requestedSchema.type must be \"object\" (got {other:?})"
        ))))),
        None => Err(VmError::Thrown(VmValue::String(Rc::from(
            "mcp_elicit: requestedSchema.type is required and must be \"object\"",
        )))),
    }
}

/// Parse the client's response into the canonical `{action, content?}`
/// envelope. When the action is `accept`, the `content` field is
/// validated against `requested_schema` so scripts can rely on it.
pub(crate) fn envelope_from_response(
    result: &JsonValue,
    requested_schema: &JsonValue,
) -> Result<VmValue, VmError> {
    let action = result
        .get("action")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "mcp_elicit: client response missing 'action'",
            )))
        })?;
    if !matches!(action, "accept" | "decline" | "cancel") {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "mcp_elicit: client response action must be 'accept'/'decline'/'cancel' (got {action:?})"
        )))));
    }

    let mut envelope: BTreeMap<String, VmValue> = BTreeMap::new();
    envelope.insert("action".to_string(), VmValue::String(Rc::from(action)));

    if action == "accept" {
        let content = result
            .get("content")
            .cloned()
            .unwrap_or(JsonValue::Object(Default::default()));
        let validated = validate_accepted_content(&content, requested_schema)?;
        envelope.insert("content".to_string(), validated);
    }

    Ok(VmValue::Dict(Rc::new(envelope)))
}

/// Validate the `content` field of an `accept` response against the
/// JSON-Schema-shaped `requestedSchema`. Returns the canonicalized VM
/// value on success.
pub(crate) fn validate_accepted_content(
    content: &JsonValue,
    requested_schema: &JsonValue,
) -> Result<VmValue, VmError> {
    let canonical_schema = elicitation_validate_schema(&json_to_vm_value(requested_schema))
        .map_err(|error| match error {
            VmError::Thrown(VmValue::String(s)) => VmError::Thrown(VmValue::String(Rc::from(
                format!("mcp_elicit: invalid requestedSchema: {s}"),
            ))),
            other => other,
        })?;
    let content_vm = json_to_vm_value(content);
    elicitation_validate(&content_vm, &canonical_schema).map_err(|error| match error {
        VmError::Thrown(VmValue::String(s)) => VmError::Thrown(VmValue::String(Rc::from(format!(
            "mcp_elicit: content failed schema validation: {s}"
        )))),
        other => other,
    })
}

thread_local! {
    static CURRENT_BUS: RefCell<Option<ElicitationBus>> = const { RefCell::new(None) };
}

/// Install `bus` as the elicitation bus for the current thread. Returns
/// the previously-installed bus (so callers can restore on exit).
pub fn install_bus(bus: Option<ElicitationBus>) -> Option<ElicitationBus> {
    CURRENT_BUS.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), bus))
}

/// Snapshot the bus currently installed on this thread, if any.
pub fn current_bus() -> Option<ElicitationBus> {
    CURRENT_BUS.with(|cell| cell.borrow().clone())
}

/// Dispatch an inbound server-to-client `elicitation/create` request
/// (received while Harn is acting as an MCP client) and return the
/// JSON-RPC response we should send back to the server.
///
/// The implementation order matches existing HITL primitives:
///   1. If a `host_mock("mcp", "elicit", ...)` matches, use that.
///   2. Otherwise, dispatch through the installed `HostCallBridge`.
///   3. If no host can take the call, decline with a structured error
///      so the server can fall back to a sensible default.
pub(crate) async fn dispatch_inbound_elicitation(
    server_name: &str,
    request: &JsonValue,
) -> JsonValue {
    let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let message = params
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let requested_schema = params
        .get("requestedSchema")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // Surface the inbound elicitation to live observers (the ACP adapter
    // renders it as an `_harn/agentEvent` of kind `mcp_notification`) so a
    // thin client can show the prompt. This is observability only — the
    // request still resolves through the host bridge / decline fallback
    // below; the response semantics are unchanged.
    if let Some(session_id) = crate::llm::current_agent_session_id() {
        crate::agent_events::emit_event(&crate::agent_events::AgentEvent::McpNotification {
            session_id,
            server: server_name.to_string(),
            method: ELICITATION_METHOD.to_string(),
            direction: "request".to_string(),
            params,
        });
    }

    // Build the params bundle dispatched to the host bridge / mock.
    // Includes the originating server name so a single host can route
    // by source, and copies the raw schema through unmodified.
    let mut bridge_params: BTreeMap<String, VmValue> = BTreeMap::new();
    bridge_params.insert("server".to_string(), VmValue::String(Rc::from(server_name)));
    bridge_params.insert(
        "message".to_string(),
        VmValue::String(Rc::from(message.as_str())),
    );
    bridge_params.insert(
        "requestedSchema".to_string(),
        json_to_vm_value(&requested_schema),
    );

    let bridge_result = dispatch_mock_host_call("mcp", "elicit", &bridge_params)
        .or_else(|| dispatch_host_call_bridge("mcp", "elicit", &bridge_params));

    let envelope_value: JsonValue = match bridge_result {
        Some(Ok(value)) => crate::mcp::vm_value_to_serde(&value),
        Some(Err(error)) => {
            let detail = match error {
                VmError::Thrown(VmValue::String(s)) => s.to_string(),
                VmError::Thrown(other) => other.display(),
                VmError::Runtime(s) | VmError::TypeError(s) => s,
                other => format!("{other:?}"),
            };
            return crate::jsonrpc::error_response(id, -32000, &detail);
        }
        None => {
            // No host bridge installed — decline politely.
            json!({ "action": "decline" })
        }
    };

    // Coerce a few common shapes into the canonical envelope. A real
    // host may return {action, content} directly; a host that doesn't
    // know about MCP may return a bare value, in which case we treat it
    // as accept-with-content.
    let envelope = normalize_inbound_envelope(envelope_value);

    // Enforce schema validation on accept so we don't propagate garbage
    // up to the calling MCP server.
    if envelope.get("action").and_then(JsonValue::as_str) == Some("accept") {
        if let Some(content) = envelope.get("content") {
            if let Err(error) = validate_accepted_content(content, &requested_schema) {
                let detail = match error {
                    VmError::Thrown(VmValue::String(s)) => s.to_string(),
                    other => format!("{other:?}"),
                };
                return crate::jsonrpc::error_response(id, -32602, &detail);
            }
        }
    }

    crate::jsonrpc::response(id, envelope)
}

fn normalize_inbound_envelope(value: JsonValue) -> JsonValue {
    let object = match value {
        JsonValue::Object(map) => map,
        JsonValue::Null => return json!({ "action": "decline" }),
        other => {
            // Bare value — treat as accept with content.
            return json!({ "action": "accept", "content": other });
        }
    };

    if object.contains_key("action") {
        return JsonValue::Object(object);
    }
    // No action field: synthesize one based on whether content is present.
    let mut out = serde_json::Map::new();
    if object.is_empty() {
        out.insert("action".into(), JsonValue::String("decline".into()));
    } else {
        out.insert("action".into(), JsonValue::String("accept".into()));
        out.insert("content".into(), JsonValue::Object(object));
    }
    JsonValue::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_id_handles_strings_numbers_and_other() {
        assert_eq!(canonical_id(&json!("a")), "a");
        assert_eq!(canonical_id(&json!(42)), "42");
        assert_eq!(canonical_id(&json!(true)), "true");
    }

    #[test]
    fn validate_requested_schema_rejects_non_object() {
        assert!(validate_requested_schema(&json!({"type": "string"})).is_err());
        assert!(validate_requested_schema(&json!("not an object")).is_err());
    }

    #[test]
    fn validate_requested_schema_accepts_object() {
        assert!(validate_requested_schema(&json!({"type": "object"})).is_ok());
    }

    #[test]
    fn envelope_from_response_decline_omits_content() {
        let envelope =
            envelope_from_response(&json!({"action": "decline"}), &json!({"type": "object"}))
                .unwrap();
        let dict = envelope.as_dict().unwrap();
        assert_eq!(dict.get("action").unwrap().display(), "decline");
        assert!(dict.get("content").is_none());
    }

    #[test]
    fn envelope_from_response_accept_validates_content() {
        let schema = json!({
            "type": "object",
            "properties": {"choice": {"type": "string"}},
            "required": ["choice"]
        });
        let envelope = envelope_from_response(
            &json!({"action": "accept", "content": {"choice": "A"}}),
            &schema,
        )
        .unwrap();
        let dict = envelope.as_dict().unwrap();
        let content = dict.get("content").unwrap().as_dict().unwrap();
        assert_eq!(content.get("choice").unwrap().display(), "A");
    }

    #[test]
    fn envelope_from_response_accept_rejects_invalid_content() {
        let schema = json!({
            "type": "object",
            "properties": {"choice": {"type": "string"}},
            "required": ["choice"]
        });
        let result = envelope_from_response(
            &json!({"action": "accept", "content": {"choice": 7}}),
            &schema,
        );
        assert!(result.is_err());
    }

    #[test]
    fn envelope_from_response_rejects_unknown_action() {
        let result = envelope_from_response(&json!({"action": "wat"}), &json!({"type": "object"}));
        assert!(result.is_err());
    }

    #[test]
    fn route_response_returns_false_for_request() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let bus = ElicitationBus::new(tx);
        assert!(!bus.route_response(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})));
        assert!(
            !bus.route_response(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled"}))
        );
    }

    #[test]
    fn route_response_ignores_unknown_id() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let bus = ElicitationBus::new(tx);
        assert!(!bus.route_response(&json!({"jsonrpc": "2.0", "id": "ghost", "result": {}})));
    }

    #[test]
    fn normalize_inbound_envelope_passes_action_through() {
        let v = normalize_inbound_envelope(json!({"action": "decline"}));
        assert_eq!(v["action"], json!("decline"));
    }

    #[test]
    fn normalize_inbound_envelope_synthesizes_accept_for_bare_dict() {
        let v = normalize_inbound_envelope(json!({"choice": "A"}));
        assert_eq!(v["action"], json!("accept"));
        assert_eq!(v["content"]["choice"], json!("A"));
    }

    #[test]
    fn normalize_inbound_envelope_decline_for_null() {
        let v = normalize_inbound_envelope(JsonValue::Null);
        assert_eq!(v["action"], json!("decline"));
    }

    #[tokio::test]
    async fn elicit_round_trip_validates_accept() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bus = ElicitationBus::new(tx);
        let bus_for_responder = bus.clone();
        tokio::spawn(async move {
            let outbound = rx.recv().await.expect("elicit request emitted");
            let id = outbound["id"].clone();
            assert_eq!(outbound["method"], json!(ELICITATION_METHOD));
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"action": "accept", "content": {"choice": "A"}}
            });
            assert!(bus_for_responder.route_response(&response));
        });
        let result = bus
            .elicit(
                "Pick one".to_string(),
                json!({
                    "type": "object",
                    "properties": {"choice": {"type": "string"}},
                    "required": ["choice"],
                }),
            )
            .await
            .expect("elicit succeeds");
        let dict = result.as_dict().unwrap();
        assert_eq!(dict.get("action").unwrap().display(), "accept");
    }

    #[tokio::test]
    async fn elicit_propagates_jsonrpc_error_from_client() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bus = ElicitationBus::new(tx);
        let bus_for_responder = bus.clone();
        tokio::spawn(async move {
            let outbound = rx.recv().await.expect("elicit request emitted");
            let id = outbound["id"].clone();
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "client refused"}
            });
            assert!(bus_for_responder.route_response(&response));
        });
        let result = bus
            .elicit("Pick one".to_string(), json!({"type": "object"}))
            .await;
        let err = result.expect_err("error is propagated");
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => format!("{other:?}"),
        };
        assert!(message.contains("client refused"), "got: {message}");
    }
}
