//! Shared MCP server-to-client request plumbing.
//!
//! Harn-served MCP handlers use this bus when they need to issue a JSON-RPC
//! request to the connected client and wait for that client's response.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as JsonValue};
use tokio::sync::{mpsc, oneshot};

use crate::value::{VmError, VmValue};

/// Outbound message sink — typically wraps the MCP transport's writer
/// (stdout for stdio servers, an SSE channel for HTTP servers).
pub type OutboundSender = mpsc::UnboundedSender<JsonValue>;

/// Per-connection bus that owns in-flight server-to-client requests.
///
/// Cheap to clone (every clone shares the same pending map), so the transport
/// layer can hand a copy to its reader task while the dispatch loop installs
/// the same bus thread-locally for tool/resource/prompt handlers.
#[derive(Clone)]
pub struct ClientRequestBus {
    outbound: OutboundSender,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<JsonValue>>>>,
    next_id: Arc<AtomicU64>,
}

impl ClientRequestBus {
    pub fn new(outbound: OutboundSender) -> Self {
        Self {
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Try to dispatch `msg` as a response to a pending request. Returns `true`
    /// when the message was a JSON-RPC response whose id matches an in-flight
    /// request. Otherwise the caller should treat `msg` as a new inbound
    /// request.
    pub fn route_response(&self, msg: &JsonValue) -> bool {
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
        let mut pending = self
            .pending
            .lock()
            .expect("client-request pending poisoned");
        if let Some(tx) = pending.remove(&id_key) {
            let _ = tx.send(msg.clone());
            true
        } else {
            false
        }
    }

    pub async fn request(
        &self,
        id_prefix: &str,
        method: &str,
        params: JsonValue,
        error_prefix: &str,
    ) -> Result<JsonValue, VmError> {
        let id_seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("harn-{id_prefix}-{id_seq}");
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("client-request pending poisoned")
            .insert(id.clone(), tx);

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        if self.outbound.send(request).is_err() {
            self.pending
                .lock()
                .expect("client-request pending poisoned")
                .remove(&id);
            return Err(VmError::Runtime(format!(
                "{error_prefix}: transport closed before request could be sent"
            )));
        }

        let response = match rx.await {
            Ok(value) => value,
            Err(_) => {
                return Err(VmError::Runtime(format!(
                    "{error_prefix}: transport dropped before client responded"
                )));
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
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("{error_prefix}: client error ({code}): {message}"),
            ))));
        }

        Ok(response.get("result").cloned().unwrap_or(JsonValue::Null))
    }
}

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

thread_local! {
    static CURRENT_BUS: RefCell<Option<ClientRequestBus>> = const { RefCell::new(None) };
}

pub fn install_bus(bus: Option<ClientRequestBus>) -> Option<ClientRequestBus> {
    CURRENT_BUS.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), bus))
}

pub fn current_bus() -> Option<ClientRequestBus> {
    CURRENT_BUS.with(|cell| cell.borrow().clone())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_id_handles_strings_numbers_and_other() {
        assert_eq!(canonical_id(&json!("a")), "a");
        assert_eq!(canonical_id(&json!(42)), "42");
        assert_eq!(canonical_id(&json!(true)), "true");
    }
}
