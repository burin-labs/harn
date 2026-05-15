//! ACP transport output and host-bridge client call plumbing.
use super::*;

pub(super) fn verbose_bridge_logs_enabled() -> bool {
    matches!(
        std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    ) || matches!(
        std::env::var("HARN_ACP_TRACE_CALLS").ok().as_deref(),
        Some("1")
    )
}

pub(super) fn host_call_timeout(method: &str) -> std::time::Duration {
    let configured = std::env::var("HARN_HOST_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0);
    if let Some(seconds) = configured {
        return std::time::Duration::from_secs(seconds);
    }
    if method == "host/call" {
        return std::time::Duration::from_secs(300);
    }
    std::time::Duration::from_secs(60)
}

pub(super) fn suppress_default_info_log(message: &str) -> bool {
    if verbose_bridge_logs_enabled() {
        return false;
    }
    [
        "ACP_BOOT:",
        "span_end ",
        "WORKFLOW_POLICY:",
        "HINTS:",
        "AGENT_CONTEXT:",
        "SIBLING_OUTLINES:",
        "PROVIDERS: count=",
        "AUTO: base context start",
        "AUTO: base context done",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
}

#[derive(Clone)]
pub(super) enum AcpOutput {
    Stdout(Arc<std::sync::Mutex<()>>),
    Channel(mpsc::UnboundedSender<String>),
}

impl AcpOutput {
    pub(super) fn stdout() -> Self {
        Self::Stdout(Arc::new(std::sync::Mutex::new(())))
    }

    pub(super) fn write_line(&self, line: &str) {
        match self {
            Self::Stdout(lock) => {
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(line.as_bytes());
                let _ = stdout.write_all(b"\n");
                let _ = stdout.flush();
            }
            Self::Channel(tx) => {
                let _ = tx.send(line.to_string());
            }
        }
    }
}

/// Shared state that bridge-style builtins use to communicate with the
/// ACP client (editor) over JSON-RPC.
pub(super) struct AcpBridge {
    pub(super) session_id: String,
    pub(super) output: AcpOutput,
    pub(super) pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    pub(super) next_id_counter: AtomicU64,
    pub(super) cancellation: SessionCancellation,
    /// Name of the currently executing Harn script (without .harn suffix).
    pub(super) script_name: std::sync::Mutex<String>,
    pub(super) assistant_state: std::sync::Mutex<VisibleTextState>,
}

impl AcpBridge {
    /// Write a complete JSON-RPC line to stdout.
    pub(super) fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC notification.
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` with agent_message_chunk.
    pub(super) fn send_update(&self, text: &str) {
        let (visible_text, visible_delta) = self
            .assistant_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text, true);
        let mut content = serde_json::json!({
            "type": "text",
            "text": text,
        });
        let mut content_meta = serde_json::Map::new();
        content_meta.insert(
            "visible_text".to_string(),
            serde_json::Value::String(visible_text),
        );
        content_meta.insert(
            "visible_delta".to_string(),
            serde_json::Value::String(visible_delta),
        );
        events::merge_harn_meta(&mut content, content_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": content,
                },
            }),
        );
    }

    /// Send a structured `session/update` with progress phase, message,
    /// and data. `progress` is a harn vendor-extension session-update
    /// variant; canonical ACP has no progress-phase concept, so all
    /// vendor fields ride under `update._meta.harn`.
    pub(super) fn send_progress(
        &self,
        phase: &str,
        message: &str,
        progress: Option<i64>,
        total: Option<i64>,
        data: Option<serde_json::Value>,
    ) {
        let update = progress_update(phase, message, progress, total, data);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Send a canonical ACP `plan` update. Per ACP, each plan update is
    /// a full replacement for the client's current plan state.
    #[allow(dead_code)]
    pub(super) fn send_plan(&self, entries: serde_json::Value) {
        let update = plan_update(entries);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Send a structured `session/update` with log level, message, and
    /// fields. `log` is a harn vendor-extension; canonical ACP has no
    /// log channel on the session-update stream, so all vendor fields
    /// ride under `update._meta.harn`.
    pub(super) fn send_log(&self, level: &str, message: &str, fields: Option<serde_json::Value>) {
        if level == "info" && suppress_default_info_log(message) {
            return;
        }
        let mut update = serde_json::json!({
            "sessionUpdate": "log",
        });
        let mut harn_meta = serde_json::Map::new();
        harn_meta.insert(
            "level".to_string(),
            serde_json::Value::String(level.to_string()),
        );
        harn_meta.insert(
            "message".to_string(),
            serde_json::Value::String(message.to_string()),
        );
        if let Some(f) = fields {
            harn_meta.insert("fields".to_string(), f);
        }
        events::merge_harn_meta(&mut update, harn_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Set the currently executing script name (without .harn suffix).
    pub(super) fn set_script_name(&self, name: &str) {
        *self.script_name.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }

    /// Get the current script name.
    #[allow(dead_code)]
    pub(super) fn get_script_name(&self) -> String {
        self.script_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Send a JSON-RPC request to the client and await the response.
    pub(super) async fn call_client(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        self.call_client_inner(method, params, true).await
    }

    pub(super) async fn call_client_for_cleanup(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        self.call_client_inner(method, params, false).await
    }

    async fn call_client_inner(
        &self,
        method: &str,
        params: serde_json::Value,
        abort_on_cancel: bool,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        if abort_on_cancel && self.cancellation.cancelled.load(Ordering::SeqCst) {
            return Err(harn_vm::VmError::Runtime("Cancelled".into()));
        }

        let id = self.next_id_counter.fetch_add(1, Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        if let Ok(line) = serde_json::to_string(&request) {
            self.write_line(&line);
        }

        let timeout = host_call_timeout(method);
        let cancellation = self.cancellation.clone();
        let wait_cancelled = async move {
            loop {
                if cancellation.cancelled.load(Ordering::SeqCst) {
                    return;
                }
                cancellation.notify.notified().await;
            }
        };
        tokio::pin!(wait_cancelled);

        tokio::select! {
            result = rx => {
                let msg = result
                    .map_err(|_| harn_vm::VmError::Runtime("Client closed connection".into()))?;
                if let Some(error) = msg.get("error") {
                    let message = error["message"].as_str().unwrap_or("Unknown client error");
                    Err(harn_vm::VmError::Runtime(format!(
                        "Client error: {message}"
                    )))
                } else {
                    Ok(msg["result"].clone())
                }
            }
            _ = &mut wait_cancelled, if abort_on_cancel => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(harn_vm::VmError::Runtime("Cancelled".into()))
            }
            _ = tokio::time::sleep(timeout) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(harn_vm::VmError::Runtime(format!(
                    "Client did not respond to '{method}' within {timeout:?}"
                )))
            }
        }
    }
}

pub(super) fn plan_update(entries: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "sessionUpdate": "plan",
        "entries": entries,
    })
}

pub(super) fn progress_update(
    phase: &str,
    message: &str,
    progress: Option<i64>,
    total: Option<i64>,
    data: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut update = serde_json::json!({
        "sessionUpdate": "progress",
    });
    let mut harn_meta = serde_json::Map::new();
    harn_meta.insert(
        "phase".to_string(),
        serde_json::Value::String(phase.to_string()),
    );
    harn_meta.insert(
        "message".to_string(),
        serde_json::Value::String(message.to_string()),
    );
    if let Some(p) = progress {
        harn_meta.insert("progress".to_string(), serde_json::Value::from(p));
    }
    if let Some(t) = total {
        harn_meta.insert("total".to_string(), serde_json::Value::from(t));
    }
    if let Some(d) = data {
        harn_meta.insert("data".to_string(), d);
    }
    events::merge_harn_meta(&mut update, harn_meta);
    update
}
