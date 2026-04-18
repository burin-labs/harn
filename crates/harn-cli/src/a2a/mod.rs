use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod card;
mod http;
mod rpc;
mod task;

#[cfg(test)]
mod tests;

use card::{agent_card, pipeline_name_from_path};
use http::{check_version_header, parse_http_request};
use rpc::{handle_jsonrpc, handle_streaming_request};
use task::{cancel_task, list_tasks, TaskStore};

/// Compile and execute a pipeline with the given task text, returning the
/// pipeline's printed output.
pub(super) async fn execute_pipeline(path: &str, task_text: &str) -> Result<String, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    let chunk = harn_vm::compile_source(&source)?;

    let local = tokio::task::LocalSet::new();
    let source_owned = source;
    let path_owned = path.to_string();
    let task_owned = task_text.to_string();

    local
        .run_until(async move {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            let source_parent = Path::new(&path_owned).parent().unwrap_or(Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            let store_base = project_root.as_deref().unwrap_or(source_parent);
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            if let Some(ref root) = project_root {
                vm.set_project_root(root);
            }
            vm.set_source_info(&path_owned, &source_owned);

            if let Some(p) = Path::new(&path_owned).parent() {
                if !p.as_os_str().is_empty() {
                    vm.set_source_dir(p);
                }
            }

            vm.set_global(
                "task",
                harn_vm::VmValue::String(std::rc::Rc::from(task_owned.as_str())),
            );

            vm.execute(&chunk).await.map_err(|e| e.to_string())?;
            Ok(vm.output().to_string())
        })
        .await
}

/// Write an HTTP response with the given status, content-type, and body.
pub(super) async fn write_http_response(
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
         Access-Control-Allow-Headers: Content-Type, A2A-Version\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

/// Write an SSE (Server-Sent Events) HTTP response header.
pub(super) async fn write_sse_header(
    stream: &mut (impl AsyncWriteExt + Unpin),
) -> tokio::io::Result<()> {
    let header = "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, A2A-Version\r\n\
         \r\n";
    stream.write_all(header.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Write a single SSE event.
pub(super) async fn write_sse_event(
    stream: &mut (impl AsyncWriteExt + Unpin),
    event_type: &str,
    data: &serde_json::Value,
) -> tokio::io::Result<()> {
    let json_str = serde_json::to_string(data).unwrap_or_default();
    let event = format!("event: {event_type}\ndata: {json_str}\n\n");
    stream.write_all(event.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Handle a single HTTP connection.
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    pipeline_path: &str,
    card_json: &str,
    store: &TaskStore,
) {
    let mut buf = vec![0u8; 65536];
    let n = match stream.read(&mut buf).await {
        Ok(0) => return,
        Ok(n) => n,
        Err(_) => return,
    };
    buf.truncate(n);

    // Drain the rest of the body if Content-Length exceeds the first read.
    let header_text = String::from_utf8_lossy(&buf);
    let content_length = header_text
        .lines()
        .find(|l| l.to_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

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

    let req = match parse_http_request(&buf) {
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

    match (req.method.as_str(), req.path.as_str()) {
        ("OPTIONS", _) => {
            let _ = write_http_response(&mut stream, 204, "No Content", "text/plain", b"").await;
        }
        ("GET", "/.well-known/a2a-agent") => {
            let _ = write_http_response(
                &mut stream,
                200,
                "OK",
                "application/json",
                card_json.as_bytes(),
            )
            .await;
        }
        ("POST", "/") => {
            let rpc_id = serde_json::from_str::<serde_json::Value>(&req.body)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(serde_json::Value::Null);

            if let Some(version_err) = check_version_header(&req.headers, &rpc_id) {
                let resp_bytes = serde_json::to_string(&version_err).unwrap_or_default();
                let _ = write_http_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/json",
                    resp_bytes.as_bytes(),
                )
                .await;
                return;
            }

            let parsed: Option<serde_json::Value> = serde_json::from_str(&req.body).ok();
            let method = parsed
                .as_ref()
                .and_then(|v| v.get("method"))
                .and_then(|m| m.as_str())
                .unwrap_or("");

            if method == "a2a.SendStreamingMessage" {
                handle_streaming_request(&mut stream, pipeline_path, &req.body, store).await;
            } else {
                let resp = handle_jsonrpc(pipeline_path, &req.body, store).await;
                let resp_bytes = resp.as_bytes();
                let _ = write_http_response(&mut stream, 200, "OK", "application/json", resp_bytes)
                    .await;
            }
        }
        ("GET", "/tasks") => {
            let tasks = list_tasks(store, None, None);
            let body_bytes = serde_json::to_string(&tasks).unwrap_or_default();
            let _ = write_http_response(
                &mut stream,
                200,
                "OK",
                "application/json",
                body_bytes.as_bytes(),
            )
            .await;
        }
        ("GET", p) if p.starts_with("/tasks/") => {
            let task_id = &p["/tasks/".len()..];
            let task_json = store.lock().unwrap().get(task_id).map(|t| t.to_json());
            match task_json {
                Some(json) => {
                    let body_bytes = serde_json::to_string(&json).unwrap_or_default();
                    let _ = write_http_response(
                        &mut stream,
                        200,
                        "OK",
                        "application/json",
                        body_bytes.as_bytes(),
                    )
                    .await;
                }
                None => {
                    let _ = write_http_response(
                        &mut stream,
                        404,
                        "Not Found",
                        "application/json",
                        b"{\"error\":\"task not found\"}",
                    )
                    .await;
                }
            }
        }
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/cancel") => {
            let task_id = &p["/tasks/".len()..p.len() - "/cancel".len()];
            let result = cancel_task(store, task_id);
            match result {
                Ok(json) => {
                    let body_bytes = serde_json::to_string(&json).unwrap_or_default();
                    let _ = write_http_response(
                        &mut stream,
                        200,
                        "OK",
                        "application/json",
                        body_bytes.as_bytes(),
                    )
                    .await;
                }
                Err(msg) => {
                    let status = if msg.contains("not found") { 404 } else { 409 };
                    let status_text = if status == 404 {
                        "Not Found"
                    } else {
                        "Conflict"
                    };
                    let err_body = serde_json::json!({"error": msg}).to_string();
                    let _ = write_http_response(
                        &mut stream,
                        status,
                        status_text,
                        "application/json",
                        err_body.as_bytes(),
                    )
                    .await;
                }
            }
        }
        _ => {
            let _ = write_http_response(&mut stream, 404, "Not Found", "text/plain", b"Not Found")
                .await;
        }
    }
}

/// Start the A2A server for a pipeline file.
pub async fn run_a2a_server(pipeline_path: &str, port: u16) {
    let path = Path::new(pipeline_path);
    if !path.exists() {
        eprintln!("Error: file not found: {pipeline_path}");
        std::process::exit(1);
    }

    let name = pipeline_name_from_path(pipeline_path);
    let card = agent_card(&name, port);
    let card_json = serde_json::to_string_pretty(&card).unwrap_or_default();

    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));

    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error: could not bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    println!("Harn A2A server listening on http://localhost:{port}");
    println!("Agent card: http://localhost:{port}/.well-known/a2a-agent");
    println!("Pipeline: {pipeline_path}");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let pipeline = pipeline_path.to_string();
                let card = card_json.clone();
                // Handled inline (not spawned): the VM is !Send because it
                // runs on a LocalSet, so requests are serialized.
                handle_connection(stream, &pipeline, &card, &store).await;
            }
            Err(e) => {
                eprintln!("Accept error: {e}");
            }
        }
    }
}
