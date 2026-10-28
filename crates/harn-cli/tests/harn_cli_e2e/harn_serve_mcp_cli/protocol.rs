use serde_json::{json, Value as JsonValue};

pub(super) const PROTOCOL_VERSION: &str = "2026-07-28";

fn stable_meta(capabilities: JsonValue) -> JsonValue {
    json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "harn-e2e-client",
            "version": "1.0.0"
        },
        "io.modelcontextprotocol/clientCapabilities": capabilities,
    })
}

pub(crate) fn stable_request(id: u64, method: &str, mut params: JsonValue) -> JsonValue {
    params["_meta"] = stable_meta(json!({
        "elicitation": {"form": {}, "url": {}},
        "roots": {},
        "sampling": {},
    }));
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
