use crate::cli::{
    ConformanceHelperArgs, ConformanceHelperBridgeMockHostArgs, ConformanceHelperCommand,
    ConformanceHelperHttpProxyArgs,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::timeout;

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize)]
struct CapturedProxyRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: String,
}

pub(crate) async fn run(args: ConformanceHelperArgs) -> Result<(), String> {
    match args.command {
        ConformanceHelperCommand::BridgeMockHost(args) => run_bridge_mock_host(args).await,
        ConformanceHelperCommand::HttpProxy(args) => run_http_proxy(args).await,
    }
}

async fn run_bridge_mock_host(args: ConformanceHelperBridgeMockHostArgs) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("resolve current harn binary: {error}"))?;
    let mut child = Command::new(current_exe);
    child.arg("run").arg("--bridge").arg(&args.pipeline);
    for arg in &args.arg {
        child.arg("--arg").arg(arg);
    }
    child
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child
        .spawn()
        .map_err(|error| format!("spawn bridge child: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "bridge child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "bridge child stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "bridge child stderr unavailable".to_string())?;

    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        let mut lines = Vec::new();
        while let Some(line) = reader.next_line().await? {
            lines.push(line);
        }
        Ok::<Vec<String>, std::io::Error>(lines)
    });

    let mut outputs = Vec::new();
    let mut errors = Vec::new();
    let mut reader = BufReader::new(stdout).lines();
    while let Some(line) = reader
        .next_line()
        .await
        .map_err(|error| format!("read bridge child stdout: {error}"))?
    {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message = match serde_json::from_str::<BridgeMessage>(line) {
            Ok(message) => message,
            Err(_) => {
                errors.push(format!("Invalid JSON from VM: {line}"));
                continue;
            }
        };
        match bridge_response(message) {
            BridgeAction::Output(output) => outputs.push(output),
            BridgeAction::Ignore => {}
            BridgeAction::Error(error) => errors.push(error),
            BridgeAction::Respond(response) => {
                let line = serde_json::to_string(&response)
                    .map_err(|error| format!("serialize bridge response: {error}"))?;
                stdin
                    .write_all(line.as_bytes())
                    .await
                    .map_err(|error| format!("write bridge response: {error}"))?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|error| format!("write bridge response newline: {error}"))?;
                stdin
                    .flush()
                    .await
                    .map_err(|error| format!("flush bridge response: {error}"))?;
            }
        }
    }
    drop(stdin);

    let status = child
        .wait()
        .await
        .map_err(|error| format!("wait for bridge child: {error}"))?;
    let stderr_lines = stderr_task
        .await
        .map_err(|error| format!("join bridge stderr reader: {error}"))?
        .map_err(|error| format!("read bridge child stderr: {error}"))?;

    for output in outputs {
        print!("{output}");
    }
    if !errors.is_empty() {
        for error in errors {
            eprintln!("ERROR: {error}");
        }
        return Err("bridge mock host observed protocol errors".to_string());
    }
    if !status.success() {
        for line in stderr_lines {
            eprintln!("{line}");
        }
        return Err(format!("bridge child exited with {status}"));
    }
    Ok(())
}

async fn run_http_proxy(args: ConformanceHelperHttpProxyArgs) -> Result<(), String> {
    let state_dir = PathBuf::from(args.state_dir);
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| format!("create state dir {}: {error}", state_dir.display()))?;
    let port_path = state_dir.join("port");
    let state_path = state_dir.join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind proxy listener: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("read listener addr: {error}"))?
        .port();
    std::fs::write(&port_path, port.to_string())
        .map_err(|error| format!("write port file {}: {error}", port_path.display()))?;

    let limit = Duration::from_millis(args.timeout_ms);
    let (mut stream, _) = timeout(limit, listener.accept())
        .await
        .map_err(|_| {
            format!(
                "timed out waiting for proxy request after {}ms",
                args.timeout_ms
            )
        })?
        .map_err(|error| format!("accept proxy request: {error}"))?;
    let request = timeout(limit, read_proxy_request(&mut stream))
        .await
        .map_err(|_| {
            format!(
                "timed out reading proxy request after {}ms",
                args.timeout_ms
            )
        })??;
    let state = serde_json::to_string(&request)
        .map_err(|error| format!("serialize proxy state: {error}"))?;
    std::fs::write(&state_path, state)
        .map_err(|error| format!("write state file {}: {error}", state_path.display()))?;

    write_proxy_response(&mut stream)
        .await
        .map_err(|error| format!("write proxy response: {error}"))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct BridgeMessage {
    id: Option<serde_json::Value>,
    method: Option<String>,
    #[serde(default)]
    params: serde_json::Value,
}

enum BridgeAction {
    Respond(serde_json::Value),
    Output(String),
    Error(String),
    Ignore,
}

fn bridge_response(message: BridgeMessage) -> BridgeAction {
    let method = message.method.as_deref().unwrap_or_default();
    let Some(id) = message.id else {
        return bridge_notification_action(method, &message.params);
    };
    let response = match method {
        "llm_call" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "text": format!(
                    "Mock LLM response to: {}",
                    message
                        .params
                        .get("prompt")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                ),
                "input_tokens": 100,
                "output_tokens": 50,
            },
        }),
        "tool_execute" => tool_execute_response(id, &message.params),
        "host_call" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": format!(
                "Mock host_call result for: {}",
                message
                    .params
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            ),
        }),
        "agent_loop" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "status": "done",
                "text": format!(
                    "Mock agent_loop result for: {}",
                    message
                        .params
                        .get("prompt")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                ),
                "iterations": 1,
                "duration_ms": 0,
                "tools_used": [],
            },
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("Unknown method: {method}")},
        }),
    };
    BridgeAction::Respond(response)
}

fn bridge_notification_action(method: &str, params: &serde_json::Value) -> BridgeAction {
    match method {
        "output" => BridgeAction::Output(
            params
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        "progress" => BridgeAction::Ignore,
        "error" => BridgeAction::Error(
            params
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        ),
        _ => BridgeAction::Ignore,
    }
}

fn tool_execute_response(id: serde_json::Value, params: &serde_json::Value) -> serde_json::Value {
    let name = params
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .and_then(serde_json::Value::as_object);
    match name {
        "read_file" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": format!(
                    "Mock content of {}",
                    arguments
                        .and_then(|arguments| arguments.get("path"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?")
                )
            },
        }),
        "exec" | "run_command" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"output": "mock command output", "exit_code": 0},
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"ok": true},
        }),
    }
}

async fn read_proxy_request<R>(stream: &mut R) -> Result<CapturedProxyRequest, String>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|error| format!("read proxy request: {error}"))?;
        if read == 0 {
            return Err("request closed before headers".to_string());
        }
        buffer.extend_from_slice(&temp[..read]);
        if buffer.len() > MAX_CAPTURE_BYTES {
            return Err(format!(
                "request exceeded {MAX_CAPTURE_BYTES} byte capture limit"
            ));
        }
        if let Some(offset) = find_double_crlf(&buffer) {
            break offset + 4;
        }
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| "missing request line".to_string())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "missing request method".to_string())?
        .to_string();
    let target = request_parts
        .next()
        .ok_or_else(|| "missing request target".to_string())?
        .to_string();
    let _version = request_parts
        .next()
        .ok_or_else(|| "missing request version".to_string())?;

    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("malformed header line: {line}"))?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|error| format!("invalid content-length '{value}': {error}"))?,
        None => 0,
    };
    if content_length > MAX_CAPTURE_BYTES {
        return Err(format!(
            "body exceeded {MAX_CAPTURE_BYTES} byte capture limit"
        ));
    }
    let body_end = header_end
        .checked_add(content_length)
        .ok_or_else(|| "request body length overflow".to_string())?;
    while buffer.len() < body_end {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|error| format!("read proxy request body: {error}"))?;
        if read == 0 {
            return Err("request closed before body".to_string());
        }
        buffer.extend_from_slice(&temp[..read]);
        if buffer.len() > header_end + MAX_CAPTURE_BYTES {
            return Err(format!(
                "request exceeded {MAX_CAPTURE_BYTES} byte capture limit"
            ));
        }
    }

    Ok(CapturedProxyRequest {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&buffer[header_end..body_end]).to_string(),
    })
}

async fn write_proxy_response(stream: &mut TcpStream) -> std::io::Result<()> {
    let payload = b"proxied";
    let headers = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raw_proxy_capture_preserves_absolute_target_and_auth() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            client
                .write_all(
                    b"GET http://example.invalid/proxy-check HTTP/1.1\r\n\
Host: example.invalid\r\n\
Proxy-Authorization: Basic dXNlcjpwYXNz\r\n\
\r\n",
                )
                .await
                .expect("write request");
        });

        let request = read_proxy_request(&mut server).await.expect("request");
        writer.await.expect("writer task");

        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "http://example.invalid/proxy-check");
        assert_eq!(
            request
                .headers
                .get("proxy-authorization")
                .map(String::as_str),
            Some("Basic dXNlcjpwYXNz")
        );
        assert_eq!(request.body, "");
    }

    #[tokio::test]
    async fn raw_proxy_capture_reads_declared_body() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            client
                .write_all(
                    b"POST http://example.invalid/proxy-check HTTP/1.1\r\n\
Content-Length: 7\r\n\
\r\npayload",
                )
                .await
                .expect("write request");
        });

        let request = read_proxy_request(&mut server).await.expect("request");
        writer.await.expect("writer task");

        assert_eq!(request.method, "POST");
        assert_eq!(request.body, "payload");
    }

    #[test]
    fn bridge_mock_host_llm_response_is_stable() {
        let action = bridge_response(BridgeMessage {
            id: Some(serde_json::json!(7)),
            method: Some("llm_call".to_string()),
            params: serde_json::json!({"prompt": "What is 2+2?"}),
        });
        let BridgeAction::Respond(response) = action else {
            panic!("expected response");
        };
        assert_eq!(response["id"], serde_json::json!(7));
        assert_eq!(
            response["result"]["text"],
            serde_json::json!("Mock LLM response to: What is 2+2?")
        );
    }

    #[test]
    fn bridge_mock_host_read_file_response_matches_legacy_fixture() {
        let action = bridge_response(BridgeMessage {
            id: Some(serde_json::json!(3)),
            method: Some("tool_execute".to_string()),
            params: serde_json::json!({
                "name": "read_file",
                "arguments": {"path": "src/main.rs"},
            }),
        });
        let BridgeAction::Respond(response) = action else {
            panic!("expected response");
        };
        assert_eq!(
            response["result"]["content"],
            serde_json::json!("Mock content of src/main.rs")
        );
    }
}
