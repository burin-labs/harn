//! Per-tool-call cancellation registry.
//!
//! When the host or a Harn script wants to abort a *specific* in-flight tool
//! call (e.g. user clicks "stop" on a runaway `git push --force`), they reach
//! through here. The registry keys cancellation handles by
//! `(session_id, call_id)` so a caller can target one call without
//! tearing down the whole session.
//!
//! ## Lifecycle
//!
//! 1. `host_agent_dispatch_tool_call` calls [`register`] right before it
//!    runs the underlying tool, holding the returned [`Handle`].
//! 2. The dispatch wraps the tool execution in a `select!` against
//!    [`Handle::cancelled`], so a triggered cancellation drops the
//!    pending tool future immediately.
//! 3. When the dispatch returns (either normally or via cancellation),
//!    the [`Guard`] returned by [`register`] auto-unregisters on drop.
//!
//! ## Triggering cancellation
//!
//! - **Harn:** the public `cancel_in_flight_tool_call` builtin registered
//!   in [`crate::stdlib::agent_sessions`].
//! - **Bridge stdin:** the `session/cancel_tool_call` notification
//!   handled in [`crate::bridge`].
//! - **ACP:** the `session/cancel_tool_call` method exposed by
//!   `crates/harn-serve/src/adapters/acp`.
//!
//! All three paths funnel into [`cancel`].
//!
//! Storage is thread-local because the VM runs on a current-thread tokio
//! worker — bridge stdin and ACP adapter share that worker via
//! `tokio::task::LocalSet`, so they can read the same registry without
//! cross-thread sync.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// Reason + flags captured when a tool call is cancelled.
#[derive(Clone, Debug)]
pub struct CancellationDetails {
    pub reason: String,
    pub inject_reminder: bool,
}

#[derive(Debug)]
struct Inner {
    cancelled: AtomicBool,
    completed: AtomicBool,
    details: Mutex<Option<CancellationDetails>>,
    notify: Notify,
    completion_notify: Notify,
}

/// Per-tool-call cancellation handle. Cheap to clone (`Arc` inside).
#[derive(Clone, Debug)]
pub struct Handle {
    pub session_id: String,
    pub call_id: String,
    pub tool_name: String,
    inner: Arc<Inner>,
}

impl Handle {
    fn new(session_id: String, call_id: String, tool_name: String) -> Self {
        Self {
            session_id,
            call_id,
            tool_name,
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                completed: AtomicBool::new(false),
                details: Mutex::new(None),
                notify: Notify::new(),
                completion_notify: Notify::new(),
            }),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub fn is_completed(&self) -> bool {
        self.inner.completed.load(Ordering::SeqCst)
    }

    /// Mark dispatch finished — called by the [`Guard`] on drop. Wakes any
    /// caller awaiting [`Handle::completed`].
    fn mark_completed(&self) {
        if !self.inner.completed.swap(true, Ordering::SeqCst) {
            self.inner.completion_notify.notify_waiters();
        }
    }

    /// Mark this call cancelled. Returns `true` if this call was the first
    /// to trigger cancellation (the caller can then push a reminder, etc.).
    pub fn cancel(&self, reason: impl Into<String>, inject_reminder: bool) -> bool {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return false;
        }
        let reason = reason.into();
        let mut details = self
            .inner
            .details
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        *details = Some(CancellationDetails {
            reason,
            inject_reminder,
        });
        drop(details);
        self.inner.notify.notify_waiters();
        true
    }

    pub fn details(&self) -> Option<CancellationDetails> {
        self.inner
            .details
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub fn reason(&self) -> Option<String> {
        self.details().map(|d| d.reason)
    }

    /// Future that resolves when this call is cancelled. Safe to await
    /// even if cancellation has already fired (returns immediately).
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    /// Future that resolves when dispatch finishes and the [`Guard`]
    /// drops. Lets a caller observe whether a cancellation actually
    /// landed (used by `cancel_in_flight_tool_call`'s `timeout_ms`).
    pub async fn completed(&self) {
        if self.is_completed() {
            return;
        }
        let notified = self.inner.completion_notify.notified();
        if self.is_completed() {
            return;
        }
        notified.await;
    }
}

thread_local! {
    static REGISTRY: RefCell<HashMap<(String, String), Handle>> =
        RefCell::new(HashMap::new());
}

/// RAII guard that unregisters the (session_id, call_id) entry on drop
/// and signals the matching handle's `completed` future.
pub struct Guard {
    session_id: String,
    call_id: String,
    handle: Handle,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.call_id.is_empty() {
            return;
        }
        self.handle.mark_completed();
        REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .remove(&(self.session_id.clone(), self.call_id.clone()));
        });
    }
}

/// Register a fresh handle for an in-flight tool call.
///
/// Returns `None` when the call has no id (e.g. some legacy parse paths
/// omit it). The caller cannot be targeted by `cancel_in_flight_tool_call`
/// in that case, so cancellation is a no-op; that matches today's behavior
/// for the few code paths that produce id-less calls.
pub fn register(
    session_id: impl Into<String>,
    call_id: impl Into<String>,
    tool_name: impl Into<String>,
) -> Option<(Handle, Guard)> {
    let session_id = session_id.into();
    let call_id = call_id.into();
    let tool_name = tool_name.into();
    if call_id.is_empty() {
        return None;
    }
    let handle = Handle::new(session_id.clone(), call_id.clone(), tool_name);
    let guard = Guard {
        session_id: session_id.clone(),
        call_id: call_id.clone(),
        handle: handle.clone(),
    };
    REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert((session_id, call_id), handle.clone());
    });
    Some((handle, guard))
}

pub fn lookup(session_id: &str, call_id: &str) -> Option<Handle> {
    REGISTRY.with(|registry| {
        registry
            .borrow()
            .get(&(session_id.to_string(), call_id.to_string()))
            .cloned()
    })
}

/// List all in-flight handles for a session — used by introspection and
/// tests; not by the hot path.
pub fn list_for_session(session_id: &str) -> Vec<Handle> {
    REGISTRY.with(|registry| {
        registry
            .borrow()
            .values()
            .filter(|handle| handle.session_id == session_id)
            .cloned()
            .collect()
    })
}

/// Outcome returned to callers of [`cancel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelStatus {
    /// Cancellation flag was set on this call. The in-flight future will
    /// observe it and unwind.
    Cancelled,
    /// The call was already cancelled by an earlier request.
    AlreadyCancelled,
    /// No in-flight call matches (session_id, call_id) right now —
    /// either it never started, or it already completed.
    NotFound,
}

impl CancelStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::AlreadyCancelled => "already_cancelled",
            Self::NotFound => "not_found",
        }
    }
}

/// Result of a cancellation request: the status plus, when a handle was
/// found, the tool's name and the live handle (so callers can wait for
/// the dispatch to actually unwind via [`Handle::completed`]).
#[derive(Clone, Debug)]
pub struct CancelOutcome {
    pub status: CancelStatus,
    pub tool_name: Option<String>,
    pub handle: Option<Handle>,
}

/// Trigger cancellation for one in-flight tool call.
pub fn cancel(
    session_id: &str,
    call_id: &str,
    reason: impl Into<String>,
    inject_reminder: bool,
) -> CancelOutcome {
    let Some(handle) = lookup(session_id, call_id) else {
        return CancelOutcome {
            status: CancelStatus::NotFound,
            tool_name: None,
            handle: None,
        };
    };
    let tool_name = Some(handle.tool_name.clone());
    let status = if handle.cancel(reason, inject_reminder) {
        CancelStatus::Cancelled
    } else {
        CancelStatus::AlreadyCancelled
    };
    CancelOutcome {
        status,
        tool_name,
        handle: Some(handle),
    }
}

/// Drop every in-flight cancellation handle on this thread. A handle is
/// normally unregistered by its [`Guard`] on drop, but an abandoned
/// dispatch (test timeout, panic during teardown) can leave a
/// `(session_id, call_id)` entry behind. The per-test reset path calls
/// this so a reused worker thread does not accumulate stale handles
/// across the suite.
pub fn reset_registry() {
    REGISTRY.with(|registry| registry.borrow_mut().clear());
}

/// Number of in-flight cancellation handles on this thread. Test-only.
#[cfg(test)]
pub fn registry_len() -> usize {
    REGISTRY.with(|registry| registry.borrow().len())
}

#[cfg(test)]
pub fn clear_registry_for_test() {
    reset_registry();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_resolves_cancelled_future() {
        clear_registry_for_test();
        let (handle, _guard) = register("sess_1", "call_1", "bash").expect("registered");
        assert!(!handle.is_cancelled());
        let cancelled = handle.cancel("user requested stop", false);
        assert!(cancelled);
        // Awaiting after the fact should resolve immediately.
        handle.cancelled().await;
        assert!(handle.is_cancelled());
        assert_eq!(handle.reason().as_deref(), Some("user requested stop"));
    }

    #[tokio::test]
    async fn cancel_returns_not_found_when_missing() {
        clear_registry_for_test();
        let outcome = cancel("sess_unknown", "call_unknown", "irrelevant", false);
        assert_eq!(outcome.status, CancelStatus::NotFound);
        assert_eq!(outcome.tool_name, None);
    }

    #[tokio::test]
    async fn cancel_is_idempotent() {
        clear_registry_for_test();
        let (_handle, _guard) = register("sess", "call_2", "shell").expect("registered");
        let first = cancel("sess", "call_2", "first", false);
        let second = cancel("sess", "call_2", "second", false);
        assert_eq!(first.status, CancelStatus::Cancelled);
        assert_eq!(second.status, CancelStatus::AlreadyCancelled);
        assert_eq!(first.tool_name.as_deref(), Some("shell"));
        assert_eq!(second.tool_name.as_deref(), Some("shell"));
    }

    #[tokio::test]
    async fn guard_unregisters_on_drop() {
        clear_registry_for_test();
        {
            let _registration = register("sess", "call_g", "tool").expect("registered");
            assert!(lookup("sess", "call_g").is_some());
        }
        assert!(lookup("sess", "call_g").is_none());
    }

    #[test]
    fn cancelled_wakes_pending_waiter() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            clear_registry_for_test();
            let (handle, _guard) = register("sess", "call_w", "tool").expect("registered");
            let waiter_handle = handle.clone();
            let task = tokio::task::spawn_local(async move { waiter_handle.cancelled().await });
            tokio::task::yield_now().await;
            handle.cancel("stopping", false);
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .expect("task should resolve quickly")
                .expect("task did not panic");
        });
    }

    #[tokio::test]
    async fn list_for_session_filters_by_session() {
        clear_registry_for_test();
        let _r1 = register("sess_a", "call_x", "tool").expect("registered");
        let _r2 = register("sess_a", "call_y", "tool").expect("registered");
        let _r3 = register("sess_b", "call_x", "tool").expect("registered");
        let mut ids: Vec<String> = list_for_session("sess_a")
            .into_iter()
            .map(|h| h.call_id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["call_x".to_string(), "call_y".to_string()]);
    }
}
