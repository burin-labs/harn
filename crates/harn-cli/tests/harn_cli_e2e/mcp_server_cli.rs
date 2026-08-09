// Portable across Unix and Windows: this suite drives `harn mcp serve` over
// piped stdio and tears the child down with `std::process::Child::kill`
// (TerminateProcess on Windows / SIGKILL on Unix), so it does not rely on
// POSIX signals or platform-specific shellouts.

use crate::test_util;

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;
use test_util::stdio_jsonrpc::StdioJsonRpcClient;

// See `harn_serve_mcp_cli::PROCESS_READY_TIMEOUT` for the rationale on the 60s
// budget — cold-starting the debug `harn` binary takes 30–40s under full
// nextest load.
const PROCESS_READY_TIMEOUT: Duration = Duration::from_mins(1);
const PROTOCOL_VERSION: &str = "2026-07-28";

fn stable_request(id: u64, method: &str, mut params: JsonValue) -> JsonValue {
    let meta = params
        .as_object_mut()
        .expect("MCP params are an object")
        .entry("_meta")
        .or_insert_with(|| json!({}));
    let meta = meta.as_object_mut().expect("MCP _meta is an object");
    meta.insert(
        "io.modelcontextprotocol/protocolVersion".into(),
        json!(PROTOCOL_VERSION),
    );
    meta.insert(
        "io.modelcontextprotocol/clientInfo".into(),
        json!({"name": "harn-e2e-client", "version": "1.0.0"}),
    );
    meta.insert(
        "io.modelcontextprotocol/clientCapabilities".into(),
        json!({}),
    );
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
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

pub fn on_ok(harness: Harness, event: TriggerEvent) -> dict {
  harness.stdio.log("ok:" + event.kind)
  return {kind: event.kind, event_id: event.id, trace_id: event.trace_id}
}

pub fn on_fail(harness: Harness, event: TriggerEvent) -> any {
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
    test_util::stdio_jsonrpc::wait_for_child_log_suffix(
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
/// [`StdioJsonRpcClient`].
#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn mcp_server_stdio_roundtrips_tools_and_resources() {
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut client = StdioJsonRpcClient::spawn("harn mcp serve", stdio_serve_command(&temp));

    let discovery = client.request(stable_request(1, "server/discover", json!({})));
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert_eq!(
        discovery["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "harn-orchestrator"
    );

    let tools = client.request(stable_request(2, "tools/list", json!({})));
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

    let fire = client.request(stable_request(
        3,
        "tools/call",
        json!({
            "name": "harn.trigger.fire",
            "arguments": { "trigger_id": "cron-ok", "payload": {} }
        }),
    ));
    assert_eq!(
        fire["result"]["structuredContent"]["status"],
        json!("dispatched")
    );

    let manifest = client.request(stable_request(
        4,
        "resources/read",
        json!({"uri": "harn://manifest"}),
    ));
    assert!(manifest["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("cron-ok"));

    client.shutdown_expect_success();
}

/// Progress notifications interleaved with a tool response are a
/// wire-specific behavior (the stdio writer funnels progress lines and the
/// final response through one ordered channel), so this stays a binary-
/// surface test — but bounded and self-diagnosing via [`StdioJsonRpcClient`].
#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn mcp_server_stdio_emits_progress_for_trigger_fire() {
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut client = StdioJsonRpcClient::spawn("harn mcp serve", stdio_serve_command(&temp));

    let _discovery = client.request(stable_request(1, "server/discover", json!({})));

    // Read everything until the id=2 response, collecting any progress
    // notifications for our token that arrive first.
    client.send(&stable_request(
        2,
        "tools/call",
        json!({
            "name": "harn.trigger.fire",
            "arguments": { "trigger_id": "cron-ok", "payload": {} },
            "_meta": { "progressToken": "fire-1" }
        }),
    ));

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
async fn mcp_server_http_discovers_and_fires_without_sessions() {
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

    let (rx, handle) = test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());
    let url = wait_for_http_listener(&mut child, &rx);
    let client = reqwest::Client::new();

    let discovery_request = stable_request(1, "server/discover", json!({}));
    let discovery = client
        .post(&url)
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION)
        .header("Mcp-Method", "server/discover")
        .json(&discovery_request)
        .send()
        .await
        .unwrap();
    assert!(discovery.status().is_success());
    assert!(discovery.headers().get("mcp-session-id").is_none());
    let discovery_json: JsonValue = discovery.json().await.unwrap();
    assert_eq!(
        discovery_json["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "harn-orchestrator"
    );

    let fire_request = stable_request(
        2,
        "tools/call",
        json!({
            "name": "harn.trigger.fire",
            "arguments": { "trigger_id": "cron-ok", "payload": {} }
        }),
    );
    let fire = client
        .post(&url)
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION)
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "harn.trigger.fire")
        .json(&fire_request)
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
    let mut client =
        StdioJsonRpcClient::spawn("hung server", command).with_timeout(Duration::from_secs(1));
    // `sleep` ignores stdin and never writes stdout, so the response read
    // must trip the deadline and panic with the diagnostic.
    let _ = client.request(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
}
