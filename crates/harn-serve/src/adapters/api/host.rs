use super::{AcpJsonRpcError, ApiState};
use serde_json::{json, Value};

/// Optional embedder implementation of `host/capabilities` and `host/call`.
///
/// Permission requests remain owned by the API permission service. Implementors
/// must enforce their concrete operation's authority and return structured ACP
/// errors for refused or unsupported operations. Calls run on the blocking pool.
pub trait ApiHostResponder: Send + Sync + 'static {
    fn respond(&self, method: &str, params: &Value) -> Result<Value, AcpJsonRpcError>;
}

impl ApiState {
    pub(super) fn handle_host_request(&self, id: Value, method: String, params: Value) {
        let state = self.clone();
        // Host I/O must not prevent permission responses, cancellation,
        // or other sessions' notifications from reaching the API.
        tokio::spawn(async move {
            let result = if let Some(responder) = state.host_responder.clone() {
                tokio::task::spawn_blocking(move || responder.respond(&method, &params))
                    .await
                    .unwrap_or_else(|_| {
                        Err(AcpJsonRpcError {
                            code: -32603,
                            message: "Embedder host responder failed".into(),
                            data: None,
                        })
                    })
            } else if method == "host/capabilities" {
                Ok(json!({}))
            } else {
                Err(AcpJsonRpcError {
                    code: -32601,
                    message: "No embedder host responder configured".into(),
                    data: None,
                })
            };
            let response = match result {
                Ok(value) => json!({"jsonrpc":"2.0", "id":id, "result":value}),
                Err(error) => json!({"jsonrpc":"2.0", "id":id, "error":error}),
            };
            state.acp.send_raw(response);
        });
    }
}
