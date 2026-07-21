// Portable across Unix and Windows: this suite drives `harn serve mcp` over
// piped stdio and tears each child down by closing stdin or calling
// `std::process::Child::kill` (TerminateProcess on Windows / SIGKILL on Unix),
// so it does not rely on POSIX signals or platform-specific shellouts.

mod test_util;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;
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
const PROCESS_READY_TIMEOUT: Duration = Duration::from_mins(1);
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

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
  let ticks = 0
  while !is_cancelled() {
    ticks = ticks + 1
    mcp_report_progress(ticks, {message: "spinning " + label})
    sleep(1ms)
  }
  return "cancelled:" + label
}
"#,
    )
    .unwrap();
}

fn write_script_surface_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("script_surface.harn"),
        r##"
pipeline main(task) {
  let tools = tool_registry()
  tools = tool_define(tools, "echo", "Echo input", {
    parameters: {text: "string"},
    returns: {type: "string"},
    handler: { args -> args.text },
    annotations: {title: "Echo Tool", readOnlyHint: true, idempotentHint: true, openWorldHint: false},
    icons: [{src: "https://example.com/echo.png", mimeType: "image/png", sizes: ["48x48"]}]
  })
  tools = tool_define(tools, "ramp", "Emit progress milestones and return a summary", {
    parameters: {steps: "int"},
    returns: {type: "string"},
    handler: { args ->
      let sent = 0
      let i = 0
      while i < args.steps {
        i = i + 1
        if mcp_report_progress(i, {total: args.steps, message: "step " + to_string(i)}) {
          sent = sent + 1
        }
      }
      "ramp:" + to_string(args.steps) + ":sent=" + to_string(sent)
    }
  })
  mcp_tools(tools)

  mcp_resource({
    uri: "docs://readme",
    name: "README",
    description: "Project readme",
    mime_type: "text/markdown",
    text: "# Hello from MCP"
  })

  mcp_resource_template({
    uri_template: "config://{key}",
    name: "Config",
    description: "Config values",
    mime_type: "text/plain",
    completions: {key: {values: ["name"], complete: { request -> ["version"] }}},
    handler: { args -> "value:" + args.key }
  })

  mcp_prompt({
    name: "review",
    description: "Review prompt",
    arguments: [
      {name: "code", required: true},
      {name: "language", required: false, suggestions: ["rust", "ruby", "typescript"]},
    ],
    handler: { args -> "Review this: " + args.code }
  })
}
"##,
    )
    .unwrap();
    fs::write(
        temp.path().join("card.json"),
        r#"{"name":"script-card","version":"1.0.0"}"#,
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
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(message) if predicate(&message) => return message,
            Ok(message) => {
                observed.push(message);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
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
    test_util::stdio_jsonrpc::wait_for_child_log_suffix(
        child,
        rx,
        "MCP workflow server ready on ",
        PROCESS_READY_TIMEOUT,
        "HTTP MCP server",
    )
}

fn send_stdio_request(
    stdin: &mut impl Write,
    rx: &Receiver<JsonValue>,
    request: JsonValue,
) -> JsonValue {
    let id = request.get("id").cloned();
    writeln!(stdin, "{}", serde_json::to_string(&request).unwrap()).unwrap();
    stdin.flush().unwrap();
    recv_until(rx, PROCESS_READY_TIMEOUT, |message| {
        id.as_ref().is_some_and(|id| message.get("id") == Some(id))
    })
}

fn parse_sse_messages(body: &str) -> Vec<JsonValue> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn parse_http_messages(body: &str) -> Vec<JsonValue> {
    let messages = parse_sse_messages(body);
    if !messages.is_empty() {
        return messages;
    }
    serde_json::from_str(body)
        .map(|message| vec![message])
        .unwrap_or_default()
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

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn serve_mcp_stdio_lists_calls_and_cancels_exported_functions() {
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = harn_e2e_command()
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
        test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());

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

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn serve_mcp_preserves_explicit_pipeline_exit_code() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("exit.harn"),
        "pipeline main(task) {\n  __io_eprintln(\"before explicit exit\")\n  exit(17)\n}\n",
    )
    .unwrap();

    let output = harn_e2e_command()
        .current_dir(temp.path())
        .args(["serve", "mcp", "exit.harn"])
        .output()
        .expect("run harn serve mcp");

    assert_eq!(output.status.code(), Some(17));
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP transport"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("before explicit exit"),
        "pipeline stderr must flush before exit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn serve_mcp_stdio_exposes_script_registered_surface() {
    let temp = TempDir::new().unwrap();
    write_script_surface_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--card")
        .arg("card.json")
        .arg("script_surface.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let (rx, stdout_handle) = spawn_stdout_reader(child.stdout.take().unwrap());
    let (_stderr_rx, _stderr_handle) =
        test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());

    let init = send_stdio_request(
        &mut stdin,
        &rx,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "stdio-test", "version": "1.0.0" }
            }
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["card"]["name"], "script-card");

    let tools = send_stdio_request(
        &mut stdin,
        &rx,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    );
    let tool_names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(tool_names.contains(&"echo"), "tools={tool_names:?}");
    assert!(tool_names.contains(&"ramp"), "tools={tool_names:?}");
    assert_eq!(tools["result"]["tools"][0]["name"], "echo");
    assert_eq!(
        tools["result"]["tools"][0]["outputSchema"]["type"],
        "string"
    );
    assert_eq!(
        tools["result"]["tools"][0]["annotations"],
        json!({
            "title": "Echo Tool",
            "readOnlyHint": true,
            "idempotentHint": true,
            "openWorldHint": false,
        })
    );
    assert_eq!(
        tools["result"]["tools"][0]["icons"][0]["src"],
        "https://example.com/echo.png"
    );

    let resources = send_stdio_request(
        &mut stdin,
        &rx,
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {}}),
    );
    let resource_uris = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(resource_uris.contains(&"well-known://mcp-card"));
    assert!(resource_uris.contains(&"docs://readme"));

    let templates = send_stdio_request(
        &mut stdin,
        &rx,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "resources/templates/list",
            "params": {}
        }),
    );
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        "config://{key}"
    );

    let prompts = send_stdio_request(
        &mut stdin,
        &rx,
        json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/list", "params": {}}),
    );
    assert_eq!(prompts["result"]["prompts"][0]["name"], "review");

    let prompt_completion = send_stdio_request(
        &mut stdin,
        &rx,
        json!({
            "jsonrpc": "2.0",
            "id": 16,
            "method": "completion/complete",
            "params": {
                "ref": {"type": "ref/prompt", "name": "review"},
                "argument": {"name": "language", "value": "ru"}
            }
        }),
    );
    assert_eq!(
        prompt_completion["result"]["completion"]["values"],
        json!(["ruby", "rust"])
    );

    let resource_completion = send_stdio_request(
        &mut stdin,
        &rx,
        json!({
            "jsonrpc": "2.0",
            "id": 17,
            "method": "completion/complete",
            "params": {
                "ref": {"type": "ref/resource", "uri": "config://{key}"},
                "argument": {"name": "key", "value": "ver"}
            }
        }),
    );
    assert_eq!(
        resource_completion["result"]["completion"]["values"],
        json!(["version"])
    );

    let prompt = send_stdio_request(
        &mut stdin,
        &rx,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "prompts/get",
            "params": {"name": "review", "arguments": {"code": "fn main() {}"}}
        }),
    );
    assert!(prompt["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("fn main"));

    // Script-defined `ramp` tool emits N progress notifications per
    // call when the client opts in via _meta.progressToken.
    writeln!(
        &mut stdin,
        "{}",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "ramp",
                "arguments": {"steps": 3},
                "_meta": {"progressToken": "ramp-token"}
            }
        }))
        .unwrap()
    )
    .unwrap();
    stdin.flush().unwrap();
    let mut ramp_progress: Vec<JsonValue> = Vec::new();
    let ramp_response = loop {
        let message = recv_until(&rx, TEST_TIMEOUT, |message| {
            message["id"] == 7
                || (message["method"] == "notifications/progress"
                    && message["params"]["progressToken"] == "ramp-token")
        });
        if message["id"] == 7 {
            break message;
        }
        ramp_progress.push(message);
    };
    assert_eq!(ramp_progress.len(), 3);
    assert_eq!(ramp_progress[0]["params"]["progress"], json!(1.0));
    assert_eq!(ramp_progress[2]["params"]["progress"], json!(3.0));
    assert_eq!(ramp_progress[2]["params"]["total"], json!(3.0));
    assert_eq!(
        ramp_response["result"]["structuredContent"],
        json!("ramp:3:sent=3")
    );

    // Without a progressToken, mcp_report_progress no-ops — the
    // structuredContent reports zero notifications were sent.
    let no_progress = send_stdio_request(
        &mut stdin,
        &rx,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "ramp",
                "arguments": {"steps": 2}
            }
        }),
    );
    assert_eq!(
        no_progress["result"]["structuredContent"],
        json!("ramp:2:sent=0")
    );

    drop(stdin);
    let status = child.wait().unwrap();
    stdout_handle.join().expect("stdout reader thread");
    assert!(status.success(), "status={status}");
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[tokio::test(flavor = "multi_thread")]
async fn serve_mcp_http_streams_progress_and_enforces_api_keys() {
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = harn_e2e_command()
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

    let (rx, handle) = test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());
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
        .header("authorization", "Bearer secret-token")
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
    let tool_names: Vec<&str> = tools_json["result"]["tools"]
        .as_array()
        .expect("tools list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(tool_names.contains(&"greet"), "tools={tool_names:?}");

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
    let unauthorized_messages = parse_http_messages(&unauthorized_body);
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
    let messages = parse_http_messages(&body);
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
    let greet_messages = parse_http_messages(&greet_body);
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

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[tokio::test(flavor = "multi_thread")]
async fn serve_mcp_http_exposes_script_registered_surface() {
    let temp = TempDir::new().unwrap();
    write_script_surface_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--transport")
        .arg("http")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--card")
        .arg("card.json")
        .arg("script_surface.harn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let (rx, handle) = test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());
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
        init_json["result"]["serverInfo"]["card"]["name"],
        "script-card"
    );

    let resources = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    let resources_json: JsonValue = resources.json().await.unwrap();
    let resource_uris = resources_json["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(resource_uris.contains(&"well-known://mcp-card"));
    assert!(resource_uris.contains(&"docs://readme"));

    let templates = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/templates/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    let templates_json: JsonValue = templates.json().await.unwrap();
    assert_eq!(
        templates_json["result"]["resourceTemplates"][0]["uriTemplate"],
        "config://{key}"
    );

    let prompts = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "prompts/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    let prompts_json: JsonValue = prompts.json().await.unwrap();
    assert_eq!(prompts_json["result"]["prompts"][0]["name"], "review");

    let prompt = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "prompts/get",
            "params": {"name": "review", "arguments": {"code": "fn main() {}"}}
        }))
        .send()
        .await
        .unwrap();
    let prompt_json: JsonValue = prompt.json().await.unwrap();
    assert!(prompt_json["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("fn main"));

    child.kill().unwrap();
    child.wait().unwrap();
    handle.join().unwrap();
}

// The registered-surface fixture mirrors
// `conformance/helpers/mcp_http_elicit_server.harn` — the surface the flaky
// loopback elicitation scenario exercises. (`mcp_elicit` is only available
// to handlers registered via `mcp_tools`.)
fn write_elicit_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
pipeline default() {
  mcp_server_metadata(
    {
      name: "harn-http-elicit-race-test",
      version: "1.0.0",
      instructions: "Use the ask tool to exercise client elicitation.",
    },
  )
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "ask",
    "Ask the client for deployment input",
    {
      parameters: {prompt: "string"},
      handler: { args ->
        const response = mcp_elicit(
          {
            message: args.prompt ?? "Choose environment",
            requestedSchema: {
              type: "object",
              properties: {env: {type: "string"}, confirm: {type: "boolean"}},
              required: ["env", "confirm"],
            },
          },
        )
        const content = response.content ?? {}
        return to_string(response.action ?? "") + ":" + to_string(content.env ?? "") + ":"
          + to_string(content.confirm ?? "")
      },
    },
  )
  mcp_tools(tools)
}
"#,
    )
    .unwrap();
}

/// Read the session GET/SSE stream until a JSON message satisfying
/// `predicate` arrives. The stream is infinite (keep-alives), so parse
/// incrementally instead of buffering the whole body; the whole read is
/// bounded by one outer timeout rather than a wall-clock poll loop.
async fn read_stream_until<F>(response: reqwest::Response, predicate: F) -> JsonValue
where
    F: Fn(&JsonValue) -> bool,
{
    use futures::StreamExt as _;
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let matched = tokio::time::timeout(PROCESS_READY_TIMEOUT, async {
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { return None };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            for line in buffer.clone().lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(msg) = serde_json::from_str::<JsonValue>(data) {
                        if predicate(&msg) {
                            return Some(msg);
                        }
                    }
                }
            }
        }
        None
    })
    .await;
    match matched {
        Ok(Some(msg)) => msg,
        Ok(None) | Err(_) => {
            panic!("timed out waiting for a matching stream message; got: {buffer}")
        }
    }
}

// Regression for the POST-vs-GET session race: a `tools/call` that needs
// elicitation arrives BEFORE the client opens its GET event stream. The
// session bus must queue the `elicitation/create` until the stream
// registers instead of failing the call because no stream existed at the
// moment the POST was processed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_mcp_http_elicits_when_tools_call_beats_the_get_stream() {
    let temp = TempDir::new().unwrap();
    write_elicit_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--transport")
        .arg("http")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("server.harn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let (rx, handle) = test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());
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
                "clientInfo": { "name": "http-elicit-race-test", "version": "1.0.0" }
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

    // Fire the eliciting tools/call FIRST — no GET stream exists yet.
    // On the racy implementation this failed immediately with "no client
    // connection"; the fix queues the elicitation until the stream opens.
    let call_task = tokio::spawn({
        let client = client.clone();
        let url = url.clone();
        let session_id = session_id.clone();
        async move {
            let response = client
                .post(&url)
                .header("Accept", "application/json, text/event-stream")
                .header("mcp-session-id", &session_id)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "ask",
                        "arguments": { "prompt": "Choose deploy target" }
                    }
                }))
                .send()
                .await
                .unwrap();
            response.text().await.unwrap()
        }
    });

    // Wait until the server has actually queued the elicitation with no
    // stream open — the delivery task logs that state — then open the GET
    // stream late. This orders the race deterministically with no timing
    // dependence: on the racy implementation the elicitation failed
    // instead of queueing, so this line never appears. The blocking wait
    // is safe because the spawned POST progresses on the second worker.
    test_util::stdio_jsonrpc::wait_for_child_log_suffix(
        &mut child,
        &rx,
        "queueing a server-to-client request",
        PROCESS_READY_TIMEOUT,
        "elicitation queue",
    );
    let stream_response = client
        .get(&url)
        .header("Accept", "text/event-stream")
        .header("mcp-session-id", &session_id)
        .send()
        .await
        .unwrap();
    assert!(stream_response.status().is_success());

    let elicitation = read_stream_until(stream_response, |msg| {
        msg.get("method").and_then(JsonValue::as_str) == Some("elicitation/create")
    })
    .await;
    let elicitation_id = elicitation["id"].clone();
    assert_eq!(
        elicitation["params"]["message"],
        json!("Choose deploy target")
    );

    // Reply to the elicitation by POSTing the JSON-RPC response back.
    let reply = client
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": elicitation_id,
            "result": { "action": "accept", "content": { "env": "staging", "confirm": true } }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), reqwest::StatusCode::ACCEPTED);

    let call_body = call_task.await.unwrap();
    let call_messages = parse_http_messages(&call_body);
    let call_result = call_messages
        .iter()
        .find(|msg| msg["id"] == json!(2))
        .unwrap_or_else(|| panic!("no tools/call response in: {call_body}"));
    let text = call_result["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("unexpected tools/call response: {call_result}"));
    assert_eq!(text, "accept:staging:true");

    child.kill().unwrap();
    child.wait().unwrap();
    handle.join().unwrap();
}
