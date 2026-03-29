//! JSON-RPC 2.0 bridge for host communication.
//!
//! When `harn run --bridge` is used, the VM delegates builtins (llm_call,
//! file I/O, tool execution) to a host process over stdin/stdout JSON-RPC.
//! The host (e.g., Burin IDE) handles these requests using its own providers.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::sync::{oneshot, Mutex};

use crate::value::{VmError, VmValue};

/// Default timeout for bridge calls (5 minutes).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// A JSON-RPC 2.0 bridge to a host process over stdin/stdout.
///
/// The bridge sends requests to the host on stdout and receives responses
/// on stdin. A background task reads stdin and dispatches responses to
/// waiting callers by request ID. All stdout writes are serialized through
/// a mutex to prevent interleaving.
pub struct HostBridge {
    next_id: AtomicU64,
    /// Pending request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Whether the host has sent a cancel notification.
    cancelled: Arc<AtomicBool>,
    /// Mutex protecting stdout writes to prevent interleaving.
    stdout_lock: Arc<std::sync::Mutex<()>>,
}

// Default doesn't apply — new() spawns async tasks requiring a tokio LocalSet.
#[allow(clippy::new_without_default)]
impl HostBridge {
    /// Create a new bridge and spawn the stdin reader task.
    ///
    /// Must be called within a tokio LocalSet (uses spawn_local for the
    /// stdin reader since it's single-threaded).
    pub fn new() -> Self {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(AtomicBool::new(false));

        // Stdin reader: reads JSON-RPC lines and dispatches responses
        let pending_clone = pending.clone();
        let cancelled_clone = cancelled.clone();
        tokio::task::spawn_local(async move {
            let stdin = tokio::io::stdin();
            let reader = tokio::io::BufReader::new(stdin);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let msg: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue, // Skip malformed lines
                };

                // Check if this is a notification from the host (no id)
                if msg.get("id").is_none() {
                    if let Some(method) = msg["method"].as_str() {
                        if method == "cancel" {
                            cancelled_clone.store(true, Ordering::SeqCst);
                        }
                    }
                    continue;
                }

                // This is a response — dispatch to the waiting caller
                if let Some(id) = msg["id"].as_u64() {
                    let mut pending = pending_clone.lock().await;
                    if let Some(sender) = pending.remove(&id) {
                        let _ = sender.send(msg);
                    }
                }
            }

            // stdin closed — cancel any remaining pending requests by dropping senders
            let mut pending = pending_clone.lock().await;
            pending.clear();
        });

        Self {
            next_id: AtomicU64::new(1),
            pending,
            cancelled,
            stdout_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// Create a bridge from pre-existing shared state.
    ///
    /// Unlike `new()`, does **not** spawn a stdin reader — the caller is
    /// responsible for dispatching responses into `pending`.  This is used
    /// by ACP mode which already has its own stdin reader.
    pub fn from_parts(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        cancelled: Arc<AtomicBool>,
        stdout_lock: Arc<std::sync::Mutex<()>>,
        start_id: u64,
    ) -> Self {
        Self {
            next_id: AtomicU64::new(start_id),
            pending,
            cancelled,
            stdout_lock,
        }
    }

    /// Write a complete JSON-RPC line to stdout, serialized through a mutex.
    fn write_line(&self, line: &str) -> Result<(), VmError> {
        let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(line.as_bytes())
            .map_err(|e| VmError::Runtime(format!("Bridge write error: {e}")))?;
        stdout
            .write_all(b"\n")
            .map_err(|e| VmError::Runtime(format!("Bridge write error: {e}")))?;
        stdout
            .flush()
            .map_err(|e| VmError::Runtime(format!("Bridge flush error: {e}")))?;
        Ok(())
    }

    /// Send a JSON-RPC request to the host and wait for the response.
    /// Times out after 5 minutes to prevent deadlocks.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        if self.is_cancelled() {
            return Err(VmError::Runtime("Bridge: operation cancelled".into()));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        // Register a oneshot channel to receive the response
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Send the request (serialized through stdout mutex)
        let line = serde_json::to_string(&request)
            .map_err(|e| VmError::Runtime(format!("Bridge serialization error: {e}")))?;
        if let Err(e) = self.write_line(&line) {
            // Clean up pending entry on write failure
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            return Err(e);
        }

        // Wait for the response with timeout
        let response = match tokio::time::timeout(DEFAULT_TIMEOUT, rx).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(_)) => {
                // Sender dropped — host closed or stdin reader exited
                return Err(VmError::Runtime(
                    "Bridge: host closed connection before responding".into(),
                ));
            }
            Err(_) => {
                // Timeout — clean up pending entry
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(VmError::Runtime(format!(
                    "Bridge: host did not respond to '{method}' within {}s",
                    DEFAULT_TIMEOUT.as_secs()
                )));
            }
        };

        // Check for JSON-RPC error
        if let Some(error) = response.get("error") {
            let message = error["message"].as_str().unwrap_or("Unknown host error");
            let code = error["code"].as_i64().unwrap_or(-1);
            return Err(VmError::Runtime(format!("Host error ({code}): {message}")));
        }

        Ok(response["result"].clone())
    }

    /// Send a JSON-RPC notification to the host (no response expected).
    /// Serialized through the stdout mutex to prevent interleaving.
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if let Ok(line) = serde_json::to_string(&notification) {
            let _ = self.write_line(&line);
        }
    }

    /// Check if the host has sent a cancel notification.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Send an output notification (for log/print in bridge mode).
    pub fn send_output(&self, text: &str) {
        self.notify("output", serde_json::json!({"text": text}));
    }

    /// Send a progress notification.
    pub fn send_progress(&self, phase: &str, message: &str) {
        self.notify(
            "progress",
            serde_json::json!({"phase": phase, "message": message}),
        );
    }
}

/// Convert a serde_json::Value to a VmValue.
pub fn json_result_to_vm_value(val: &serde_json::Value) -> VmValue {
    crate::stdlib::json_to_vm_value(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_request_format() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "llm_call",
            "params": {
                "prompt": "Hello",
                "system": "Be helpful",
            },
        });
        let s = serde_json::to_string(&request).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"method\":\"llm_call\""));
    }

    #[test]
    fn test_json_rpc_notification_format() {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "output",
            "params": {"text": "[harn] hello\n"},
        });
        let s = serde_json::to_string(&notification).unwrap();
        assert!(s.contains("\"method\":\"output\""));
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn test_json_rpc_error_response_parsing() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32600,
                "message": "Invalid request",
            },
        });
        assert!(response.get("error").is_some());
        assert_eq!(
            response["error"]["message"].as_str().unwrap(),
            "Invalid request"
        );
    }

    #[test]
    fn test_json_rpc_success_response_parsing() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "text": "Hello world",
                "input_tokens": 10,
                "output_tokens": 5,
            },
        });
        assert!(response.get("result").is_some());
        assert_eq!(response["result"]["text"].as_str().unwrap(), "Hello world");
    }

    #[test]
    fn test_cancelled_flag() {
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(!cancelled.load(Ordering::SeqCst));
        cancelled.store(true, Ordering::SeqCst);
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn test_json_result_to_vm_value_string() {
        let val = serde_json::json!("hello");
        let vm_val = json_result_to_vm_value(&val);
        assert_eq!(vm_val.display(), "hello");
    }

    #[test]
    fn test_json_result_to_vm_value_dict() {
        let val = serde_json::json!({"name": "test", "count": 42});
        let vm_val = json_result_to_vm_value(&val);
        let VmValue::Dict(d) = &vm_val else {
            unreachable!("Expected Dict, got {:?}", vm_val);
        };
        assert_eq!(d.get("name").unwrap().display(), "test");
        assert_eq!(d.get("count").unwrap().display(), "42");
    }

    #[test]
    fn test_json_result_to_vm_value_null() {
        let val = serde_json::json!(null);
        let vm_val = json_result_to_vm_value(&val);
        assert!(matches!(vm_val, VmValue::Nil));
    }

    #[test]
    fn test_json_result_to_vm_value_nested() {
        let val = serde_json::json!({
            "text": "response",
            "tool_calls": [
                {"id": "tc_1", "name": "read_file", "arguments": {"path": "foo.rs"}}
            ],
            "input_tokens": 100,
            "output_tokens": 50,
        });
        let vm_val = json_result_to_vm_value(&val);
        let VmValue::Dict(d) = &vm_val else {
            unreachable!("Expected Dict, got {:?}", vm_val);
        };
        assert_eq!(d.get("text").unwrap().display(), "response");
        let VmValue::List(list) = d.get("tool_calls").unwrap() else {
            unreachable!("Expected List for tool_calls");
        };
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_timeout_duration() {
        assert_eq!(DEFAULT_TIMEOUT.as_secs(), 300);
    }
}
