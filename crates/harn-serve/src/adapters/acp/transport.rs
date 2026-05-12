//! ACP stdio and in-process channel transport loops.
use super::*;

pub async fn run_acp_channel_server(
    config: AcpServerConfig,
    mut request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) {
    let profile_enabled = config.profile.is_enabled();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(response_tx));
            let pending_clone = server.pending.clone();
            let cancellations = server.session_cancellations.clone();
            let (routed_tx, mut routed_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            tokio::task::spawn_local(async move {
                while let Some(msg) = request_rx.recv().await {
                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    prepare_session_prompt(&cancellations, &msg);
                    if preempt_session_cancel(&cancellations, &msg) {
                        continue;
                    }

                    let _ = routed_tx.send(msg);
                }

                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = routed_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
    if profile_enabled {
        harn_vm::tracing::set_tracing_enabled(false);
    }
}

/// Start the ACP server. Reads JSON-RPC from stdin, writes to stdout.
pub async fn run_acp_server(config: AcpServerConfig) {
    let profile_enabled = config.profile.is_enabled();
    let local = tokio::task::LocalSet::new();

    local
        .run_until(async move {
            let mut server = AcpServer::new(config);

            // stdin dispatcher: routes responses to pending waiters, and
            // requests/notifications onto the request channel.
            let pending_clone = server.pending.clone();
            let cancellations = server.session_cancellations.clone();
            let (request_tx, mut request_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            eprintln!("[harn] ACP workflow server ready on stdio");

            tokio::task::spawn_local(async move {
                let stdin = tokio::io::stdin();
                let reader = tokio::io::BufReader::new(stdin);
                let mut lines = reader.lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }

                    let msg: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    prepare_session_prompt(&cancellations, &msg);
                    if preempt_session_cancel(&cancellations, &msg) {
                        continue;
                    }

                    let _ = request_tx.send(msg);
                }

                // stdin closed — clean up pending.
                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = request_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
    if profile_enabled {
        harn_vm::tracing::set_tracing_enabled(false);
    }
}
