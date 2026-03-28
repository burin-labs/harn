use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use harn_lexer::Lexer;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

/// Global task ID counter.
static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate the Agent Card JSON for a pipeline file.
fn agent_card(pipeline_name: &str, port: u16) -> serde_json::Value {
    serde_json::json!({
        "name": pipeline_name,
        "description": "Harn pipeline agent",
        "url": format!("http://localhost:{port}"),
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "streaming": false,
            "pushNotifications": false
        },
        "skills": [
            {
                "id": "execute",
                "name": "Execute Pipeline",
                "description": "Run the harn pipeline with a task"
            }
        ],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    })
}

/// Extract the pipeline name from a .harn file path (stem without extension).
fn pipeline_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string()
}

/// Compile and execute a pipeline with the given task text, returning the
/// pipeline's printed output.
async fn execute_pipeline(path: &str, task_text: &str) -> Result<String, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| e.to_string())?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        if diag.severity == DiagnosticSeverity::Error {
            return Err(diag.message.clone());
        }
    }

    let chunk = harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| e.to_string())?;

    let local = tokio::task::LocalSet::new();
    let source_owned = source;
    let path_owned = path.to_string();
    let task_owned = task_text.to_string();

    local
        .run_until(async move {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            harn_vm::register_http_builtins(&mut vm);
            harn_vm::register_llm_builtins(&mut vm);
            vm.set_source_info(&path_owned, &source_owned);

            if let Some(p) = Path::new(&path_owned).parent() {
                if !p.as_os_str().is_empty() {
                    vm.set_source_dir(p);
                }
            }

            // Inject the task text as the pipeline parameter
            vm.set_global(
                "task",
                harn_vm::VmValue::String(std::rc::Rc::from(task_owned.as_str())),
            );

            vm.execute(&chunk).await.map_err(|e| e.to_string())?;
            Ok(vm.output().to_string())
        })
        .await
}

/// Build a JSON-RPC success response wrapping an A2A Task object.
fn task_response(rpc_id: &serde_json::Value, task_text: &str, output: &str) -> serde_json::Value {
    let task_id = format!("task-{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "id": task_id,
            "status": {"state": "completed"},
            "history": [
                {
                    "role": "user",
                    "parts": [{"type": "text", "text": task_text}]
                },
                {
                    "role": "agent",
                    "parts": [{"type": "text", "text": output.trim_end()}]
                }
            ]
        }
    })
}

/// Build a JSON-RPC error response.
fn error_response(rpc_id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

/// Parse an HTTP request from raw bytes. Returns (method, path, body).
fn parse_http_request(raw: &[u8]) -> Option<(String, String, String)> {
    let text = String::from_utf8_lossy(raw);

    // Split headers from body
    let (header_section, body) = if let Some(pos) = text.find("\r\n\r\n") {
        (&text[..pos], text[pos + 4..].to_string())
    } else if let Some(pos) = text.find("\n\n") {
        (&text[..pos], text[pos + 2..].to_string())
    } else {
        return None;
    };

    let request_line = header_section.lines().next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    Some((method, path, body))
}

/// Write an HTTP response with the given status, content-type, and body.
async fn write_http_response(
    stream: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    status_text: &str,
    content_type: &str,
    body: &[u8],
) -> tokio::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

/// Handle a single HTTP connection.
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    pipeline_path: &str,
    card_json: &str,
) {
    let mut buf = vec![0u8; 65536];
    let n = match stream.read(&mut buf).await {
        Ok(0) => return,
        Ok(n) => n,
        Err(_) => return,
    };
    buf.truncate(n);

    // If headers indicate a Content-Length larger than what we read, read more.
    let header_text = String::from_utf8_lossy(&buf);
    let content_length = header_text
        .lines()
        .find(|l| l.to_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    // Find where the body starts
    let header_end = header_text
        .find("\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| header_text.find("\n\n").map(|p| p + 2))
        .unwrap_or(n);

    let body_so_far = n.saturating_sub(header_end);
    if body_so_far < content_length {
        let remaining = content_length - body_so_far;
        let mut extra = vec![0u8; remaining];
        let mut read_total = 0;
        while read_total < remaining {
            match stream.read(&mut extra[read_total..]).await {
                Ok(0) => break,
                Ok(nr) => read_total += nr,
                Err(_) => break,
            }
        }
        buf.extend_from_slice(&extra[..read_total]);
    }

    let (method, path, body) = match parse_http_request(&buf) {
        Some(parsed) => parsed,
        None => {
            let _ = write_http_response(
                &mut stream,
                400,
                "Bad Request",
                "text/plain",
                b"Bad Request",
            )
            .await;
            return;
        }
    };

    match (method.as_str(), path.as_str()) {
        // CORS preflight
        ("OPTIONS", _) => {
            let _ = write_http_response(&mut stream, 204, "No Content", "text/plain", b"").await;
        }
        // Agent Card endpoint
        ("GET", "/.well-known/agent.json") => {
            let _ = write_http_response(
                &mut stream,
                200,
                "OK",
                "application/json",
                card_json.as_bytes(),
            )
            .await;
        }
        // A2A JSON-RPC endpoint
        ("POST", "/") => {
            let resp = handle_jsonrpc(pipeline_path, &body).await;
            let resp_bytes = resp.as_bytes();
            let _ =
                write_http_response(&mut stream, 200, "OK", "application/json", resp_bytes).await;
        }
        _ => {
            let _ = write_http_response(&mut stream, 404, "Not Found", "text/plain", b"Not Found")
                .await;
        }
    }
}

/// Handle a JSON-RPC request body, returning the JSON response string.
async fn handle_jsonrpc(pipeline_path: &str, body: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let resp = error_response(
                &serde_json::Value::Null,
                -32700,
                &format!("Parse error: {e}"),
            );
            return serde_json::to_string(&resp).unwrap_or_default();
        }
    };

    let rpc_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let resp = match method {
        "message/send" => {
            // Extract the task text from the message parts
            let task_text = parsed
                .pointer("/params/message/parts")
                .and_then(|parts| parts.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|p| {
                        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                            p.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or("");

            if task_text.is_empty() {
                error_response(
                    &rpc_id,
                    -32602,
                    "Invalid params: no text part found in message",
                )
            } else {
                match execute_pipeline(pipeline_path, task_text).await {
                    Ok(output) => task_response(&rpc_id, task_text, &output),
                    Err(e) => error_response(&rpc_id, -32000, &format!("Pipeline error: {e}")),
                }
            }
        }
        _ => error_response(&rpc_id, -32601, &format!("Method not found: {method}")),
    };

    serde_json::to_string(&resp).unwrap_or_default()
}

/// Start the A2A server for a pipeline file.
pub async fn run_a2a_server(pipeline_path: &str, port: u16) {
    // Verify the pipeline file exists and is parseable before starting
    let path = Path::new(pipeline_path);
    if !path.exists() {
        eprintln!("Error: file not found: {pipeline_path}");
        std::process::exit(1);
    }

    let name = pipeline_name_from_path(pipeline_path);
    let card = agent_card(&name, port);
    let card_json = serde_json::to_string_pretty(&card).unwrap_or_default();

    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error: could not bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    println!("Harn A2A server listening on http://localhost:{port}");
    println!("Agent card: http://localhost:{port}/.well-known/agent.json");
    println!("Pipeline: {pipeline_path}");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let pipeline = pipeline_path.to_string();
                let card = card_json.clone();
                // Each connection is handled inline (not spawned) because the
                // VM uses LocalSet and is !Send. For a simple A2A server this
                // is fine -- requests are handled sequentially.
                handle_connection(stream, &pipeline, &card).await;
            }
            Err(e) => {
                eprintln!("Accept error: {e}");
            }
        }
    }
}
