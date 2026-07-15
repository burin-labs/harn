use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;
use tokio::sync::{broadcast, mpsc};

use super::types::{ConnectionState, McpOrchestratorService};
use harn_serve::transport::{
    read_jsonrpc_stdio_frame, write_jsonrpc_stdio_message, JsonRpcStdioFrameStyle,
};

pub(super) async fn run_stdio(service: Arc<McpOrchestratorService>) -> Result<(), String> {
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut session = ConnectionState::default();
    let mut list_notifications = service.subscribe_list_notifications();
    let mut resource_notifications = service.subscribe_resource_notifications();
    let mut task_notifications = service.subscribe_task_notifications();
    let mut log_notifications = service.subscribe_log_notifications();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let input_reader = tokio::spawn(async move {
        loop {
            match read_jsonrpc_stdio_frame(&mut stdin).await {
                Ok(Some(frame)) => {
                    if in_tx.send(Ok(frame)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = in_tx.send(Err(error));
                    break;
                }
            }
        }
    });

    // Single mpsc fan-in for everything we write to stdout: per-request
    // responses, broadcast notifications, and progress updates emitted
    // by tool handlers via `harn_vm::mcp_progress`. Funnelling all
    // outbound JSON through one writer task means progress lines and
    // their final response can never interleave mid-line, and the
    // ProgressBus can hand its sender to handle_tools_call without a
    // separate channel per request.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<JsonValue>();
    let output_style = Arc::new(Mutex::new(JsonRpcStdioFrameStyle::default()));
    let writer_style = output_style.clone();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = out_rx.recv().await {
            let style = *writer_style.lock().expect("stdio frame style poisoned");
            if write_jsonrpc_stdio_message(&mut stdout, &message, style)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let progress_bus = harn_vm::mcp_progress::ProgressBus::from_mpsc(out_tx.clone());
    let _bus_guard = harn_vm::mcp_progress::ActiveBusGuard::install(Some(progress_bus));

    eprintln!("[harn] MCP stdio server ready");

    loop {
        tokio::select! {
            frame = in_rx.recv() => {
                let Some(frame) = frame else {
                    break;
                };
                let frame = frame?;
                *output_style.lock().expect("stdio frame style poisoned") = frame.style;
                let request: JsonValue = match frame.parse_json() {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                let response = service.handle_request(&mut session, request).await;
                if !response.is_null() {
                    let _ = out_tx.send(response);
                }
            }
            notification = list_notifications.recv() => {
                match notification {
                    Ok(notification) => { let _ = out_tx.send(notification); }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = resource_notifications.recv() => {
                match notification {
                    Ok(notification) if session.subscribed_resources.contains(&notification.uri) => {
                        let _ = out_tx.send(notification.message);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = task_notifications.recv() => {
                match notification {
                    Ok(notification) if notification.owner == session.client_identity => {
                        let _ = out_tx.send(notification.message);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            notification = log_notifications.recv() => {
                match notification {
                    Ok(notification) if notification.level >= session.log_level => {
                        let _ = out_tx.send(notification.message);
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    // Drop both senders for `out_tx` (the loop's clone and the
    // ProgressBus's clone, held by the install guard) so the writer
    // task observes a closed channel and exits — otherwise it would
    // block on `recv()` forever and the awaited join would hang.
    drop(_bus_guard);
    drop(out_tx);
    let _ = writer.await;
    input_reader.abort();
    let _ = input_reader.await;
    Ok(())
}
