use super::ApiState;
use serde_json::{json, Value};

impl ApiState {
    /// Answer a host request that reached the API because no embedder host
    /// responder is configured; a configured one answers inside the ACP bridge.
    pub(super) fn handle_host_request(&self, id: Value, method: String) {
        let response = if method == "host/capabilities" {
            json!({"jsonrpc":"2.0", "id":id, "result":{}})
        } else {
            json!({"jsonrpc":"2.0", "id":id, "error":{
                "code": -32601,
                "message": "No embedder host responder configured",
            }})
        };
        self.acp.send_raw(response);
    }
}
