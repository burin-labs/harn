//! Free-standing ACP writers shared between the server and bridge.
//!
//! These helpers bypass `AcpServer`/`AcpBridge` and write directly through
//! the transport output sink. They are used from contexts where we only hold
//! the sink (e.g. cancellation/error paths).

use super::AcpOutput;

/// Write a JSON-RPC response directly through a transport output sink.
pub(super) fn send_json_response(
    output: &AcpOutput,
    id: &serde_json::Value,
    result: serde_json::Value,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    if let Ok(line) = serde_json::to_string(&response) {
        output.write_line(&line);
    }
}
