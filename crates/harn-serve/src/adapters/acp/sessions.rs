//! ACP session state and cancellation routing helpers.
use super::*;

#[derive(Clone, Default)]
pub(super) struct SessionInfo {
    pub(super) title: Option<String>,
    pub(super) meta: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone)]
pub(super) struct SessionCancellation {
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) notify: Arc<Notify>,
    /// Set by the transport reader after it resets cancellation for a
    /// prompt, so the prompt handler does not erase a cancel notification
    /// that arrived while the prompt was queued.
    prepared_prompt: Arc<AtomicBool>,
}

impl Default for SessionCancellation {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            prepared_prompt: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SessionCancellation {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(super) fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }

    pub(super) fn prepare_prompt(&self) {
        self.reset();
        self.prepared_prompt.store(true, Ordering::SeqCst);
    }

    pub(super) fn begin_prompt(&self) {
        if !self.prepared_prompt.swap(false, Ordering::SeqCst) {
            self.reset();
        }
    }
}

pub(super) struct Session {
    pub(super) cwd: PathBuf,
    /// If a cancel was requested for the current prompt execution.
    pub(super) cancellation: SessionCancellation,
    /// Active host bridge for queued input / daemon resume while a prompt runs.
    pub(super) host_bridge: Option<Rc<harn_vm::bridge::HostBridge>>,
    pub(super) info: SessionInfo,
    /// Snapshot of slash-commands most recently advertised over
    /// `available_commands_update` for this session, used to skip re-emits
    /// when the underlying pipeline source hasn't changed.
    pub(super) advertised_commands: Vec<DiscoveredCommand>,
    /// Active session mode id (one of [`modes::MODE_CATALOG`]). Drives
    /// the capability ceiling pushed for the next `session/prompt`.
    pub(super) current_mode_id: String,
    /// Prompt executions emitted to profile output for this ACP session.
    pub(super) profile_turn: u64,
}

pub(super) fn mark_cancelled_session(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    params: &serde_json::Value,
) -> bool {
    let Some(session_id) = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())
    else {
        return false;
    };
    let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) else {
        return false;
    };
    cancellation.cancel();
    true
}

pub(super) fn lookup_session_cancellation(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    session_id: &str,
) -> Option<SessionCancellation> {
    cancellations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned()
}

pub(super) fn preempt_session_interruption(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    msg: &serde_json::Value,
) -> bool {
    let method = msg.get("method").and_then(|value| value.as_str());
    let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
    match method {
        Some("session/cancel") => {
            mark_cancelled_session(cancellations, params);
            msg.get("id").is_none()
        }
        Some("session/truncate" | "session/close" | "session/stop") => {
            mark_cancelled_session(cancellations, params);
            false
        }
        _ => false,
    }
}

pub(super) fn prepare_session_prompt(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    msg: &serde_json::Value,
) {
    if msg.get("method").and_then(|value| value.as_str()) != Some("session/prompt") {
        return;
    }
    let Some(session_id) = msg
        .get("params")
        .and_then(|params| params.get("sessionId"))
        .and_then(|value| value.as_str())
    else {
        return;
    };
    if let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) {
        cancellation.prepare_prompt();
    }
}
