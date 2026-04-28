#![cfg(unix)]
// These integration tests serialize CLI child processes with a cross-process
// file lock. Nextest runs each test in a separate process, so a static mutex
// would not prevent several heavyweight `harn` servers from cold-starting at
// once and racing the protocol-level readiness assertions.
#![allow(clippy::await_holding_lock)]

#[path = "support/mcp.rs"]
mod mcp_support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;
use tokio::sync::oneshot;

// Two-tier timeout convention shared with the orchestrator integration tests:
// cold-start of the debug `harn` binary is process-bound and can stretch under
// full nextest load, while JSON-RPC roundtrips against an already-ready server
// finish in milliseconds. Use the wider budget for the first protocol response
// or HTTP readiness URL, and the tighter budget for subsequent message recvs.
//
// Empirically, cold-starting the debug `harn` binary takes 30–40s when nextest
// fans out across the full workspace and saturates every core. The 15s budget
// previously used here was tight enough that it tripped intermittently, even
// when the binary itself eventually came up healthy. Keep the protocol-level
// budget tight so logic regressions surface quickly.
const PROCESS_READY_TIMEOUT: Duration = Duration::from_secs(60);
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

fn lock_harn_serve_mcp_tests() -> mcp_support::HarnProcessTestNoLock {
    mcp_support::lock_mcp_process_tests()
}

fn write_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
pub fn greet(name: string, excited: bool = false) -> dict {
  if excited {
    return {message: "Hello, " + name + "!"}
  }
  return {message: "Hello, " + name}
}

pub fn fail(kind: string) -> string {
  throw "boom:" + kind
}

pub fn spin(label: string) -> string {
  while !is_cancelled() {
    sleep(1ms)
  }
  return "cancelled:" + label
}
"#,
    )
    .unwrap();
}

fn spawn_stdout_reader(
    stdout: impl std::io::Read + Send + 'static,
) -> (Receiver<JsonValue>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.trim().is_empty() {
                continue;
            }
            let value: JsonValue = serde_json::from_str(&line).unwrap();
            let _ = tx.send(value);
        }
    });
    (rx, handle)
}

fn recv_until<F>(rx: &Receiver<JsonValue>, timeout: Duration, predicate: F) -> JsonValue
where
    F: Fn(&JsonValue) -> bool,
{
    let deadline = Instant::now() + timeout;
    let mut observed: Vec<JsonValue> = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(message) if predicate(&message) => return message,
            Ok(message) => {
                observed.push(message);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(error) => panic!("stdout reader disconnected: {error}"),
        }
    }
    panic!(
        "timed out waiting for matching JSON-RPC message; observed {} non-matching message(s): {:?}",
        observed.len(),
        observed
    );
}

fn wait_for_http_listener(child: &mut std::process::Child, rx: &Receiver<String>) -> String {
    mcp_support::wait_for_child_log_suffix(
        child,
        rx,
        "MCP workflow server ready on ",
        PROCESS_READY_TIMEOUT,
        "HTTP MCP server",
    )
}

fn parse_sse_messages(body: &str) -> Vec<JsonValue> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn collect_sse_body_after_progress(
    mut response: reqwest::Response,
    mut progress_seen: Option<oneshot::Sender<()>>,
) -> String {
    let mut body = String::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        let chunk = std::str::from_utf8(&chunk).unwrap();
        body.push_str(chunk);
        if progress_seen.is_some()
            && parse_sse_messages(&body)
                .iter()
                .any(|message| message["method"] == "notifications/progress")
        {
            let _ = progress_seen.take().unwrap().send(());
        }
    }
    body
}

#[test]
fn serve_mcp_stdio_lists_calls_and_cancels_exported_functions() {
    let _guard = lock_harn_serve_mcp_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let (rx, stdout_handle) = spawn_stdout_reader(child.stdout.take().unwrap());
    let (_stderr_rx, _stderr_handle) =
        mcp_support::spawn_stderr_reader(child.stderr.take().unwrap());

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "stdio-test", "version": "1.0.0" }
            }
        })
    )
    .unwrap();
    let init = recv_until(&rx, PROCESS_READY_TIMEOUT, |message| message["id"] == 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "server");

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
    )
    .unwrap();
    let tools = recv_until(&rx, TEST_TIMEOUT, |message| message["id"] == 2);
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["fail", "greet", "spin"]);

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "greet",
                "arguments": { "name": "alice", "excited": true }
            }
        })
    )
    .unwrap();
    let greet = recv_until(&rx, TEST_TIMEOUT, |message| message["id"] == 3);
    assert_eq!(
        greet["result"]["structuredContent"]["message"],
        json!("Hello, alice!")
    );

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "fail",
                "arguments": { "kind": "stdio" }
            }
        })
    )
    .unwrap();
    let fail = recv_until(&rx, TEST_TIMEOUT, |message| message["id"] == 4);
    assert_eq!(fail["result"]["isError"], json!(true));
    assert!(fail.get("error").is_none());

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "spin",
                "arguments": { "label": "stdio" },
                "_meta": { "progressToken": "spin-stdio" }
            }
        })
    )
    .unwrap();
    let progress = recv_until(&rx, TEST_TIMEOUT, |message| {
        message["method"] == "notifications/progress"
    });
    assert_eq!(progress["params"]["progressToken"], json!("spin-stdio"));

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": 5,
                "reason": "test cancel"
            }
        })
    )
    .unwrap();

    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "ping",
            "params": {}
        })
    )
    .unwrap();
    let ping = recv_until(&rx, TEST_TIMEOUT, |message| message["id"] == 6);
    assert_eq!(ping["result"], json!({}));

    drop(stdin);
    let status = child.wait().unwrap();
    stdout_handle.join().expect("stdout reader thread");
    assert!(status.success(), "status={status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_mcp_http_streams_progress_and_enforces_api_keys() {
    let _guard = lock_harn_serve_mcp_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--transport")
        .arg("http")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--api-key")
        .arg("secret-token")
        .arg("server.harn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let (rx, handle) = mcp_support::spawn_stderr_reader(child.stderr.take().unwrap());
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

    let tools = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    let tools_json: JsonValue = tools.json().await.unwrap();
    assert_eq!(tools_json["result"]["tools"][1]["name"], "greet");

    let unauthorized = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "greet",
                "arguments": { "name": "no-auth" }
            }
        }))
        .send()
        .await
        .unwrap();
    let unauthorized_body = unauthorized.text().await.unwrap();
    let unauthorized_messages = parse_sse_messages(&unauthorized_body);
    assert_eq!(unauthorized_messages[0]["error"]["code"], json!(-32001));

    let (progress_tx, progress_rx) = oneshot::channel();
    let call_task = tokio::spawn({
        let client = client.clone();
        let url = url.clone();
        let session_id = session_id.clone();
        async move {
            let response = client
                .post(&url)
                .header("Accept", "application/json, text/event-stream")
                .header("mcp-session-id", &session_id)
                .header("authorization", "Bearer secret-token")
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": {
                        "name": "spin",
                        "arguments": { "label": "http" },
                        "_meta": { "progressToken": "spin-http" }
                    }
                }))
                .send()
                .await
                .unwrap();
            collect_sse_body_after_progress(response, Some(progress_tx)).await
        }
    });

    tokio::time::timeout(TEST_TIMEOUT, progress_rx)
        .await
        .expect("timed out waiting for streamed MCP progress notification")
        .expect("streaming MCP request ended before emitting progress");
    let cancel = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": 4,
                "reason": "stop"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), reqwest::StatusCode::ACCEPTED);

    let body = tokio::time::timeout(TEST_TIMEOUT, call_task)
        .await
        .expect("timed out waiting for cancelled MCP stream to close")
        .unwrap();
    let messages = parse_sse_messages(&body);
    assert!(messages
        .iter()
        .any(|message| message["method"] == "notifications/progress"));
    assert!(!messages.iter().any(|message| message["id"] == 4));

    let greet = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .header("authorization", "Bearer secret-token")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "greet",
                "arguments": { "name": "http", "excited": true }
            }
        }))
        .send()
        .await
        .unwrap();
    let greet_body = greet.text().await.unwrap();
    let greet_messages = parse_sse_messages(&greet_body);
    let final_response = greet_messages
        .iter()
        .find(|message| message["id"] == 5)
        .unwrap();
    assert_eq!(
        final_response["result"]["structuredContent"]["message"],
        json!("Hello, http!")
    );

    child.kill().unwrap();
    child.wait().unwrap();
    handle.join().unwrap();
}
