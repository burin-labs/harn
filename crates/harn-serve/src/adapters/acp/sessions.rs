//! ACP session state and cancellation routing helpers.
use super::*;

#[derive(Clone, Default)]
pub(super) struct SessionInfo {
    pub(super) title: Option<String>,
    pub(super) meta: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum SessionBudget {
    #[default]
    Inherit,
    Unlimited,
    Custom(BudgetSpec),
}

#[derive(Clone)]
pub(super) struct SessionCancellation {
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) notify: Arc<Notify>,
    routed_cancel_ack_pending: Arc<AtomicBool>,
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
            routed_cancel_ack_pending: Arc::new(AtomicBool::new(false)),
            prepared_prompt: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SessionCancellation {
    pub(super) fn cancel(&self) -> bool {
        let already_cancelled = self.cancelled.swap(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        !already_cancelled
    }

    pub(super) fn cancel_for_routed_request(&self) {
        if self.cancel() {
            self.routed_cancel_ack_pending.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn take_routed_cancel_ack(&self) -> bool {
        self.routed_cancel_ack_pending.swap(false, Ordering::SeqCst)
    }

    pub(super) fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
        self.routed_cancel_ack_pending
            .store(false, Ordering::SeqCst);
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
    /// Host bridge for the active prompt, if one is running.
    pub(super) host_bridge: Option<Arc<harn_vm::bridge::HostBridge>>,
    /// Pending user-message inject state that survives prompt cancellation
    /// and active bridge replacement until delivery or explicit revoke.
    pub(super) inject_state: harn_vm::bridge::HostBridgeInjectionState,
    pub(super) info: SessionInfo,
    /// Snapshot of slash-commands most recently advertised over
    /// `available_commands_update` for this session, used to skip re-emits
    /// when the underlying pipeline source hasn't changed.
    pub(super) advertised_commands: Vec<DiscoveredCommand>,
    /// Active session mode id (one of [`modes::MODE_CATALOG`]). Drives
    /// the capability ceiling pushed for the next `session/prompt`.
    pub(super) current_mode_id: String,
    /// Session-level budget override applied to subsequent prompt turns.
    pub(super) budget: SessionBudget,
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

pub(super) fn mark_cancelled_session_for_routed_request(
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
    cancellation.cancel_for_routed_request();
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
            if msg.get("id").is_some() {
                mark_cancelled_session_for_routed_request(cancellations, params);
                false
            } else {
                mark_cancelled_session(cancellations, params);
                true
            }
        }
        Some("session/truncate" | "session/close" | "session/stop") => {
            mark_cancelled_session(cancellations, params);
            false
        }
        _ => false,
    }
}

/// Re-arm the live LLM `call_budget` ceilings on the engine thread out-of-band,
/// so a prompt turn already in flight observes the new cap on its next LLM
/// dispatch (burin-labs/burin-code#1561). Returns `true` when `msg` was a
/// `session/set_budget` control frame (so the router drops it instead of
/// queueing it behind the active turn).
///
/// This runs on the same router task / engine thread that drives the prompt
/// turn. The cost/token ceilings are per-thread thread-locals
/// (`harn_vm::set_llm_*_budget`), which is exactly why this reaches an in-flight
/// turn: the blocked message loop would only process the frame after the turn
/// unwinds. It mirrors how `session/cancel` preempts a running turn from the
/// router.
///
/// Shape: `params = { llm_cost_usd?: number|null, llm_tokens?: number|null }`.
/// An absent field leaves that ceiling unchanged; an explicit `null` clears the
/// cap; a number re-arms it. The control re-arms the live ceiling **in place**,
/// preserving accumulated spend — it deliberately does not touch
/// `session.budget` (the per-turn `@budget` source), which would reset
/// accumulation at the next turn.
pub(super) fn apply_session_budget_rearm(msg: &serde_json::Value) -> bool {
    if msg.get("method").and_then(|value| value.as_str()) != Some("session/set_budget") {
        return false;
    }
    let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
    rearm_dimension(params.get("llm_cost_usd"), harn_vm::set_llm_cost_budget);
    rearm_dimension(params.get("llm_tokens"), |cap| {
        harn_vm::set_llm_token_budget(cap.map(|tokens| tokens.max(0.0) as u64));
    });
    true
}

/// Re-arm one `session/set_budget` ceiling dimension. An absent field
/// (`None`) or a malformed value leaves the live ceiling untouched rather than
/// guessing; an explicit JSON `null` clears the cap (`set(None)`); a finite
/// number re-arms it (`set(Some(cap))`). The `set` closure adapts the `f64`
/// ceiling to the dimension's thread-local setter.
fn rearm_dimension(value: Option<&serde_json::Value>, set: impl FnOnce(Option<f64>)) {
    match value {
        Some(serde_json::Value::Null) => set(None),
        Some(serde_json::Value::Number(number)) => {
            if let Some(cap) = number.as_f64().filter(|n| n.is_finite()) {
                set(Some(cap));
            }
        }
        _ => {}
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

#[cfg(test)]
mod budget_rearm_tests {
    use super::*;
    use serde_json::json;

    fn set_budget_frame(params: serde_json::Value) -> serde_json::Value {
        json!({ "jsonrpc": "2.0", "method": "session/set_budget", "params": params })
    }

    #[test]
    fn rearms_cost_and_token_ceilings_and_clears_with_null() {
        assert!(apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": 1.5, "llm_tokens": 50_000 })
        )));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(1.5));
        assert_eq!(harn_vm::peek_llm_token_budget(), Some(50_000));

        // An explicit null clears the cap on that dimension.
        assert!(apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": null, "llm_tokens": null })
        )));
        assert_eq!(harn_vm::peek_llm_cost_budget(), None);
        assert_eq!(harn_vm::peek_llm_token_budget(), None);
    }

    #[test]
    fn absent_field_leaves_that_dimension_untouched() {
        apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": 2.0, "llm_tokens": 100 }),
        ));
        // Re-arm cost only; the token ceiling must survive.
        apply_session_budget_rearm(&set_budget_frame(json!({ "llm_cost_usd": 3.0 })));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(3.0));
        assert_eq!(harn_vm::peek_llm_token_budget(), Some(100));
        // Leave the thread-local clean for any test reusing this worker thread.
        apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": null, "llm_tokens": null }),
        ));
    }

    #[test]
    fn ignores_non_budget_frames_and_malformed_values() {
        harn_vm::set_llm_cost_budget(Some(5.0));
        // A malformed (non-number, non-null) value leaves the ceiling untouched.
        assert!(apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": "lots" })
        )));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(5.0));
        // A different method is not our control frame.
        assert!(!apply_session_budget_rearm(&json!({
            "method": "session/prompt", "params": {}
        })));
        harn_vm::set_llm_cost_budget(None);
    }
}
