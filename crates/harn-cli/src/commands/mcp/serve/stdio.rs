use std::sync::Arc;

use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use super::types::{ConnectionState, McpOrchestratorService};

pub(super) async fn run_stdio(service: Arc<McpOrchestratorService>) -> Result<(), String> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut session = ConnectionState::default();
    let mut list_notifications = service.subscribe_list_notifications();
    let mut resource_notifications = service.subscribe_resource_notifications();
    let mut task_notifications = service.subscribe_task_notifications();
    let mut log_notifications = service.subscribe_log_notifications();

    // Single mpsc fan-in for everything we write to stdout: per-request
    // responses, broadcast notifications, and progress updates emitted
    // by tool handlers via `harn_vm::mcp_progress`. Funnelling all
    // outbound JSON through one writer task means progress lines and
    // their final response can never interleave mid-line, and the
    // ProgressBus can hand its sender to handle_tools_call without a
    // separate channel per request.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<JsonValue>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = out_rx.recv().await {
            if write_stdio_json(&mut stdout, &message).await.is_err() {
                break;
            }
        }
    });

    let progress_bus = harn_vm::mcp_progress::ProgressBus::from_mpsc(out_tx.clone());
    let _bus_guard = harn_vm::mcp_progress::ActiveBusGuard::install(Some(progress_bus));

    eprintln!("[harn] MCP stdio server ready");

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line.map_err(|error| format!("stdin read failed: {error}"))? else {
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let request: JsonValue = match serde_json::from_str(trimmed) {
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
    Ok(())
}

async fn write_stdio_json(stdout: &mut tokio::io::Stdout, value: &JsonValue) -> Result<(), String> {
    let mut encoded =
        serde_json::to_string(value).map_err(|error| format!("serialize error: {error}"))?;
    encoded.push('\n');
    stdout
        .write_all(encoded.as_bytes())
        .await
        .map_err(|error| format!("stdout write failed: {error}"))?;
    stdout
        .flush()
        .await
        .map_err(|error| format!("stdout flush failed: {error}"))
}
