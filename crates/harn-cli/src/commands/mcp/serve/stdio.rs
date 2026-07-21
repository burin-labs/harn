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
    // Explicit shutdown signal for the writer. We must not couple the
    // writer's lifetime to "every `out_tx` clone has been dropped": a
    // background task (notably an MCP task-mode tool spawned by
    // `create_tool_task`) can hold a progress sender past connection
    // shutdown, which would leave `out_rx.recv()` pending forever and wedge
    // the awaited `writer` join at EOF — an intermittent, load-dependent
    // hang to the nextest slow-test cap (harn#5397). On shutdown we instead
    // tell the writer to flush what is already queued and exit.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        loop {
            tokio::select! {
                message = out_rx.recv() => match message {
                    Some(message) => {
                        let style = *writer_style.lock().expect("stdio frame style poisoned");
                        if write_jsonrpc_stdio_message(&mut stdout, &message, style)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    None => break,
                },
                _ = &mut shutdown_rx => {
                    // Drain anything already queued so a final response is
                    // never truncated, then exit without waiting on stray
                    // sender clones.
                    while let Ok(message) = out_rx.try_recv() {
                        let style = *writer_style.lock().expect("stdio frame style poisoned");
                        if write_jsonrpc_stdio_message(&mut stdout, &message, style)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    break;
                }
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

    // Signal the writer to flush and exit. Dropping the two senders we
    // hold here (the loop's clone and the ProgressBus's clone in the
    // install guard) is not sufficient on its own, because a detached
    // background task can still hold a progress sender; the explicit
    // shutdown makes teardown independent of that. See the writer's
    // construction above for the full rationale (harn#5397).
    drop(_bus_guard);
    let _ = shutdown_tx.send(());
    drop(out_tx);
    let _ = writer.await;
    input_reader.abort();
    let _ = input_reader.await;
    Ok(())
}
