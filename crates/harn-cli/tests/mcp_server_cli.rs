#![cfg(unix)]
// Serialized via MCP_CLI_TEST_LOCK (std::sync::Mutex). Swapping to
// tokio::sync::Mutex here would require threading it through every async test
// helper; the std guard is released when each test's async runtime finishes
// so holding it across awaits is safe in practice.
#![allow(clippy::await_holding_lock)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;

static MCP_CLI_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock_mcp_cli_tests() -> MutexGuard<'static, ()> {
    MCP_CLI_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap()
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

fn send_request(
    stdin: &mut impl Write,
    stdout: &mut BufReader<impl std::io::Read>,
    request: JsonValue,
) -> JsonValue {
    writeln!(stdin, "{}", serde_json::to_string(&request).unwrap()).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

fn wait_for_http_listener(child: &mut std::process::Child, rx: &Receiver<String>) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains("MCP HTTP listener ready on ") => {
                return line
                    .split("MCP HTTP listener ready on ")
                    .nth(1)
                    .unwrap()
                    .trim()
                    .to_string();
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("HTTP MCP server exited early: {status}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("HTTP MCP stderr stream closed");
            }
        }
    }
    panic!("timed out waiting for HTTP MCP listener");
}

#[test]
fn mcp_server_stdio_roundtrips_tools_and_resources() {
    let _guard = lock_mcp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .arg("mcp")
        .arg("serve")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let init = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "integration", "version": "1.0.0" }
            }
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "harn-orchestrator");

    let tools = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    );
    assert!(tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "harn.trigger.fire"));
    assert!(tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "harn.secret_scan"));

    let scan = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 22,
            "method": "tools/call",
            "params": {
                "name": "harn.secret_scan",
                "arguments": {
                    "content": r#"token = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB""#
                }
            }
        }),
    );
    assert_eq!(
        scan["result"]["structuredContent"][0]["detector"],
        json!("github-token")
    );

    let dlq_fire = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "harn.trigger.fire",
                "arguments": { "trigger_id": "cron-fail", "payload": {} }
            }
        }),
    );
    assert_eq!(
        dlq_fire["result"]["structuredContent"]["status"],
        json!("dlq")
    );
    let dlq_entry_id = dlq_fire["result"]["structuredContent"]["dlq_entry_id"]
        .as_str()
        .unwrap()
        .to_string();

    let dlq_list = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "harn.orchestrator.dlq.list",
                "arguments": {}
            }
        }),
    );
    assert_eq!(
        dlq_list["result"]["structuredContent"]["entries"][0]["id"],
        dlq_entry_id
    );

    let dlq_retry = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "harn.orchestrator.dlq.retry",
                "arguments": { "entry_id": dlq_entry_id }
            }
        }),
    );
    assert_eq!(
        dlq_retry["result"]["structuredContent"]["entry_id"],
        json!(dlq_entry_id)
    );

    let ok_fire = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "harn.trigger.fire",
                "arguments": { "trigger_id": "cron-ok", "payload": {} }
            }
        }),
    );
    assert_eq!(
        ok_fire["result"]["structuredContent"]["status"],
        json!("dispatched")
    );
    let ok_event_id = ok_fire["result"]["structuredContent"]["event_id"]
        .as_str()
        .unwrap()
        .to_string();

    let replay = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "harn.trigger.replay",
                "arguments": { "event_id": ok_event_id }
            }
        }),
    );
    assert_eq!(
        replay["result"]["structuredContent"]["status"],
        json!("dispatched")
    );

    let queue = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "harn.orchestrator.queue",
                "arguments": {}
            }
        }),
    );
    assert!(
        queue["result"]["structuredContent"]["outbox"]["count"]
            .as_u64()
            .unwrap()
            >= 1
    );

    let inspect = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "harn.orchestrator.inspect",
                "arguments": {}
            }
        }),
    );
    assert_eq!(
        inspect["result"]["structuredContent"]["triggers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let trust = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "harn.trust.query",
                "arguments": { "query": "test" }
            }
        }),
    );
    assert_eq!(trust["result"]["structuredContent"]["results"], json!([]));

    let manifest = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "resources/read",
            "params": { "uri": "harn://manifest" }
        }),
    );
    assert!(manifest["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("cron-ok"));

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "status={status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_server_http_roundtrips_initialize_and_fire() {
    let _guard = lock_mcp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = Command::new(env!("CARGO_BIN_EXE_harn"))
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

    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            let _ = tx.send(line);
        }
    });
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
