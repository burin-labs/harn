// Portable across Unix and Windows: this suite drives `harn mcp serve` over
// piped stdio and tears the child down with `std::process::Child::kill`
// (TerminateProcess on Windows / SIGKILL on Unix), so it does not rely on
// POSIX signals or platform-specific shellouts.
#![allow(clippy::await_holding_lock)]

#[path = "support/mcp.rs"]
mod mcp_support;
mod test_util;

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use mcp_support::StdioMcpClient;
use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;

// See `harn_serve_mcp_cli::PROCESS_READY_TIMEOUT` for the rationale on the 60s
// budget — cold-starting the debug `harn` binary takes 30–40s under full
// nextest load.
const PROCESS_READY_TIMEOUT: Duration = Duration::from_mins(1);

fn lock_mcp_cli_tests() -> mcp_support::HarnProcessTestNoLock {
    mcp_support::lock_mcp_process_tests()
}

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_fixture(temp: &TempDir) {
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "cron-ok"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_ok"

[[triggers]]
id = "cron-fail"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_fail"
retry = { max = 1, backoff = "immediate", retention_days = 7 }
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_ok(event: TriggerEvent) -> dict {
  log("ok:" + event.kind)
  return {kind: event.kind, event_id: event.id, trace_id: event.trace_id}
}

pub fn on_fail(event: TriggerEvent) -> any {
  throw "boom:" + event.kind
}
"#,
    );
}

fn stdio_serve_command(temp: &TempDir) -> std::process::Command {
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("mcp")
        .arg("serve")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state");
    command
}

fn wait_for_http_listener(child: &mut std::process::Child, rx: &Receiver<String>) -> String {
    mcp_support::wait_for_child_log_suffix(
        child,
        rx,
        "MCP HTTP listener ready on ",
        PROCESS_READY_TIMEOUT,
        "HTTP MCP server",
    )
}

/// Wire-level smoke for the stdio transport: prove that a real `harn mcp
/// serve` process frames JSON-RPC correctly, dispatches a tool call, reads a
/// resource, and shuts down cleanly on EOF.
///
/// This deliberately does *not* re-assert the per-tool contract of every
/// orchestrator tool (dlq, retry, replay, queue, inspect, trust, …). Those
/// are covered exhaustively and deterministically in-process against
/// `handle_request` in `serve_tests.rs`, where there is no process spawn, no
/// pipe timing, and no cold-start to flake. Duplicating them here only
/// created golden-value rot and a slow binary gauntlet that could drift
/// toward the nextest slow-test cap under load (harn#5397). The smoke keeps
/// exactly the coverage the binary surface uniquely owns — framing, real
/// dispatch, resource read, and lifecycle — via the bounded, self-diagnosing
/// [`StdioMcpClient`].
#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn mcp_server_stdio_roundtrips_tools_and_resources() {
    let _guard = lock_mcp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut client = StdioMcpClient::spawn(stdio_serve_command(&temp));

    let init = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "integration", "version": "1.0.0" }
        }
    }));
    assert_eq!(init["result"]["serverInfo"]["name"], "harn-orchestrator");

    let tools = client.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tool_names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        tool_names.contains(&"harn.trigger.fire") && tool_names.contains(&"harn.secret_scan"),
        "tools/list must advertise the orchestrator tools over the wire, got {tool_names:?}"
    );

    let fire = client.request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "harn.trigger.fire",
            "arguments": { "trigger_id": "cron-ok", "payload": {} }
        }
    }));
    assert_eq!(
        fire["result"]["structuredContent"]["status"],
        json!("dispatched")
    );

    let manifest = client.request(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/read",
        "params": { "uri": "harn://manifest" }
    }));
    assert!(manifest["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("cron-ok"));

    client.shutdown_expect_success();
}

/// Progress notifications interleaved with a tool response are a
/// wire-specific behavior (the stdio writer funnels progress lines and the
/// final response through one ordered channel), so this stays a binary-
/// surface test — but bounded and self-diagnosing via [`StdioMcpClient`].
#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn mcp_server_stdio_emits_progress_for_trigger_fire() {
    let _guard = lock_mcp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut client = StdioMcpClient::spawn(stdio_serve_command(&temp));

    let _init = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "progress-test", "version": "1.0.0" }
        }
    }));

    // Read everything until the id=2 response, collecting any progress
    // notifications for our token that arrive first.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "harn.trigger.fire",
            "arguments": { "trigger_id": "cron-ok", "payload": {} },
            "_meta": { "progressToken": "fire-1" }
        }
    }));

    let mut progress_messages = Vec::new();
    let response = client.recv_until(
        |message| {
            if message.get("method") == Some(&json!("notifications/progress"))
                && message["params"]["progressToken"] == json!("fire-1")
            {
                progress_messages.push(message.clone());
            }
        },
        |message| message.get("id") == Some(&json!(2)),
    );
    assert_eq!(
        response["result"]["structuredContent"]["status"],
        json!("dispatched")
    );
    assert!(
        !progress_messages.is_empty(),
        "expected at least one progress notification for fire-1"
    );
    let progress_values: Vec<f64> = progress_messages
        .iter()
        .map(|message| message["params"]["progress"].as_f64().unwrap())
        .collect();
    assert!(
        progress_values.windows(2).all(|w| w[1] > w[0]),
        "progress values must strictly increase, got {progress_values:?}"
    );

    client.shutdown_expect_success();
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[tokio::test(flavor = "multi_thread")]
async fn mcp_server_http_roundtrips_initialize_and_fire() {
    let _guard = lock_mcp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("mcp")
        .arg("serve")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state")
        .arg("--transport")
        .arg("http")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let (rx, handle) = mcp_support::spawn_line_reader(child.stderr.take().unwrap());
    let url = wait_for_http_listener(&mut child, &rx);
    let client = reqwest::Client::new();

    let init = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "http-test", "version": "1.0.0" }
            }
        }))
        .send()
        .await
        .unwrap();
    assert!(init.status().is_success());
    let session_id = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let init_json: JsonValue = init.json().await.unwrap();
    assert_eq!(
        init_json["result"]["serverInfo"]["name"],
        "harn-orchestrator"
    );

    let fire = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "harn.trigger.fire",
                "arguments": { "trigger_id": "cron-ok", "payload": {} }
            }
        }))
        .send()
        .await
        .unwrap();
    assert!(fire.status().is_success());
    let fire_json: JsonValue = fire.json().await.unwrap();
    assert_eq!(
        fire_json["result"]["structuredContent"]["status"],
        json!("dispatched")
    );

    child.kill().unwrap();
    child.wait().unwrap();
    handle.join().unwrap();
}

/// The safety net itself: a server that never answers must be diagnosed
/// within the bound, not blocked on until the nextest slow-test cap. Uses a
/// plain hung process (`sleep`) so the check is hermetic and fast — no
/// `harn` cold-start — and asserts the client kills it and reports rather
/// than hanging. This is the mechanism that prevents a recurrence of the
/// 180s opaque timeout (harn#5397).
#[cfg(unix)]
#[test]
#[should_panic(expected = "timed out")]
fn stdio_client_diagnoses_a_hung_server_instead_of_blocking() {
    let mut command = std::process::Command::new("sleep");
    command.arg("120");
    let mut client = StdioMcpClient::spawn(command).with_timeout(Duration::from_secs(1));
    // `sleep` ignores stdin and never writes stdout, so the response read
    // must trip the deadline and panic with the diagnostic.
    let _ = client.request(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }));
}
