//! Free-standing ACP stdout writers shared between the server and bridge.
//!
//! These helpers bypass `AcpServer`/`AcpBridge` and write directly through
//! the stdout mutex. They are used from contexts where we only hold the
//! lock (e.g. cancellation/error paths and the agent event sink).

use std::io::Write;
use std::sync::Arc;

use harn_vm::visible_text::sanitize_visible_assistant_text;

/// Write a `session/update` notification directly through a stdout lock.
pub(super) fn send_update_raw(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    session_id: &str,
    text: &str,
) {
    let visible_text = sanitize_visible_assistant_text(text, true);
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": text,
                    "visible_text": visible_text.clone(),
                    "visible_delta": visible_text,
                },
            },
        },
    });
    if let Ok(line) = serde_json::to_string(&notification) {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

/// Write a JSON-RPC response directly through a stdout lock.
pub(super) fn send_json_response(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    id: &serde_json::Value,
    result: serde_json::Value,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    if let Ok(line) = serde_json::to_string(&response) {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

/// Write a JSON-RPC error response directly through a stdout lock.
pub(super) fn send_json_error(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    id: &serde_json::Value,
    code: i64,
    message: &str,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    });
    if let Ok(line) = serde_json::to_string(&response) {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

pub(super) fn flush_stdio() {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

pub(super) fn exit_after_fatal_prompt_error(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    session_id: &str,
    id: &serde_json::Value,
    message: &str,
) -> ! {
    send_update_raw(stdout_lock, session_id, &format!("Error: {message}\n"));
    send_json_error(stdout_lock, id, -32000, message);
    eprintln!("{message}");
    flush_stdio();
    std::process::exit(2);
}
