//! MCP `notifications/progress` plumbing — server-to-client progress
//! updates emitted from a long-running tool handler.
//!
//! The MCP spec (2025-11-25) lets a client opt into progress updates by
//! attaching `_meta.progressToken` to a request. While the matching tool
//! is in flight, the server may emit any number of
//! `notifications/progress` notifications carrying the same token. The
//! server must not emit progress for requests without a token, and
//! progress values must strictly increase per token.
//!
//! Mechanics mirror the elicitation bus
//! (`crate::mcp_elicit::ElicitationBus`): a per-connection [`ProgressBus`]
//! wraps the transport's outbound JSON sink (installed thread-locally
//! via [`install_active_bus`]), and a per-call [`ProgressContext`] is
//! bound for the duration of a tool handler future via [`scope_context`]
//! (a tokio task-local) so that helpers — notably the
//! `mcp_report_progress` stdlib builtin — can find the right token
//! without taking it as an explicit argument. The split between
//! thread-local bus and task-local context matters: adapters spawn
//! concurrent tool calls onto a shared `LocalSet`, so a thread-local
//! context would race across awaits.
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress>

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as JsonValue};

/// Outbound JSON sink for progress notifications.
///
/// We accept a closure rather than a concrete `mpsc::UnboundedSender` so
/// HTTP transports — which feed an `axum::Sse` stream via a wrapping
/// closure — can install the same kind of bus as stdio without an
/// adapter shim.
pub type OutboundFn = Arc<dyn Fn(JsonValue) + Send + Sync>;

/// Per-connection progress notifier.
///
/// Cheap to clone — every clone shares the same outbound sink and the
/// same "last-progress" map used to enforce monotonicity.
#[derive(Clone)]
pub struct ProgressBus {
    outbound: OutboundFn,
    last_progress: Arc<Mutex<std::collections::HashMap<String, f64>>>,
}

impl std::fmt::Debug for ProgressBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressBus").finish_non_exhaustive()
    }
}

impl ProgressBus {
    pub fn new(outbound: OutboundFn) -> Self {
        Self {
            outbound,
            last_progress: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Convenience constructor that wraps a `tokio::sync::mpsc`
    /// unbounded sender — the shape used by the stdio MCP server.
    pub fn from_mpsc(tx: tokio::sync::mpsc::UnboundedSender<JsonValue>) -> Self {
        Self::new(Arc::new(move |message| {
            let _ = tx.send(message);
        }))
    }

    /// Emit a `notifications/progress` notification for `token`.
    ///
    /// Per spec the `progress` value MUST monotonically increase across
    /// calls with the same token. We silently drop any update that would
    /// regress so a buggy handler can't violate the contract on the
    /// wire; the user-visible builtin returns `false` in that case so
    /// scripts can detect it if they care.
    pub fn report(
        &self,
        token: &JsonValue,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> bool {
        if !is_valid_progress_token(token) {
            return false;
        }
        if !progress.is_finite() {
            return false;
        }
        if let Some(total) = total {
            if !total.is_finite() {
                return false;
            }
        }
        let key = canonical_token(token);
        {
            let mut last = self.last_progress.lock().expect("progress map poisoned");
            if let Some(previous) = last.get(&key).copied() {
                if progress <= previous {
                    return false;
                }
            }
            last.insert(key, progress);
        }
        let mut params = serde_json::Map::new();
        params.insert("progressToken".to_string(), token.clone());
        params.insert("progress".to_string(), json!(progress));
        if let Some(total) = total {
            params.insert("total".to_string(), json!(total));
        }
        if let Some(message) = message {
            params.insert("message".to_string(), JsonValue::String(message));
        }
        (self.outbound)(crate::jsonrpc::notification(
            "notifications/progress",
            JsonValue::Object(params),
        ));
        true
    }
}

/// Per-call progress context — the bus plus the token the current
/// request supplied. Bound for the lifetime of a tool handler future
/// via [`scope_context`] so that `mcp_report_progress(...)` can find it
/// without explicit threading.
#[derive(Clone, Debug)]
pub struct ProgressContext {
    pub bus: ProgressBus,
    pub token: JsonValue,
}

impl ProgressContext {
    pub fn new(bus: ProgressBus, token: JsonValue) -> Self {
        Self { bus, token }
    }

    pub fn report(&self, progress: f64, total: Option<f64>, message: Option<String>) -> bool {
        self.bus.report(&self.token, progress, total, message)
    }
}

tokio::task_local! {
    /// Per-call progress context — the token plus the bus. Bound for
    /// the lifetime of a single tool handler future via
    /// [`scope_context`]. We use a tokio task-local rather than a
    /// thread-local because adapters (e.g. `harn-serve`) spawn
    /// concurrent tool calls onto a shared `LocalSet`; a thread-local
    /// would let one task's await yield to another that overwrites the
    /// context, which is the exact race tokio task-locals are designed
    /// to avoid.
    static CURRENT_CONTEXT: ProgressContext;
}

thread_local! {
    static ACTIVE_BUS: RefCell<Option<ProgressBus>> = const { RefCell::new(None) };
}

/// Run `future` with `ctx` installed as the active progress context.
/// When `ctx` is `None`, the future runs without a context (so
/// [`current_context`] returns `None` from inside it). Use this rather
/// than installing into a thread-local: tool handlers are async and
/// concurrent on the same OS thread, so we need per-task scoping.
pub async fn scope_context<F>(ctx: Option<ProgressContext>, future: F) -> F::Output
where
    F: std::future::Future,
{
    match ctx {
        Some(ctx) => CURRENT_CONTEXT.scope(ctx, future).await,
        None => future.await,
    }
}

/// Snapshot the progress context for the current task, if any.
pub fn current_context() -> Option<ProgressContext> {
    CURRENT_CONTEXT.try_with(|ctx| ctx.clone()).ok()
}

/// Install a connection-scoped [`ProgressBus`] for the current thread.
/// Connections are single-threaded (the dispatch loop owns one OS
/// thread or LocalSet), so a thread-local is the right scope here:
/// every per-call task derived from this connection sees the same bus.
pub fn install_active_bus(bus: Option<ProgressBus>) -> Option<ProgressBus> {
    ACTIVE_BUS.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), bus))
}

/// Snapshot the active connection-scoped progress bus, if any.
pub fn active_bus() -> Option<ProgressBus> {
    ACTIVE_BUS.with(|cell| cell.borrow().clone())
}

/// RAII guard for [`install_active_bus`]. Connection-scoped, so a
/// thread-local guard is correct here.
pub struct ActiveBusGuard {
    previous: Option<ProgressBus>,
}

impl ActiveBusGuard {
    pub fn install(bus: Option<ProgressBus>) -> Self {
        Self {
            previous: install_active_bus(bus),
        }
    }
}

impl Drop for ActiveBusGuard {
    fn drop(&mut self) {
        install_active_bus(self.previous.take());
    }
}

/// MCP progress tokens are constrained to strings or numbers (no nulls,
/// objects, arrays, or booleans). Validate at the boundary so we never
/// echo a malformed token back to the client.
pub fn is_valid_progress_token(value: &JsonValue) -> bool {
    matches!(value, JsonValue::String(_) | JsonValue::Number(_))
}

/// Coerce JSON-RPC tokens (strings or numbers) into a single string key
/// for the per-token monotonicity check.
fn canonical_token(value: &JsonValue) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(n) = value.as_i64() {
        return n.to_string();
    }
    if let Some(n) = value.as_u64() {
        return n.to_string();
    }
    if let Some(n) = value.as_f64() {
        return n.to_string();
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn capturing_bus() -> (ProgressBus, Arc<Mutex<Vec<JsonValue>>>) {
        let captured: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_sink = captured.clone();
        let bus = ProgressBus::new(Arc::new(move |message| {
            captured_for_sink
                .lock()
                .expect("captured progress poisoned")
                .push(message);
        }));
        (bus, captured)
    }

    #[test]
    fn reports_progress_with_monotonic_check() {
        let (bus, captured) = capturing_bus();
        assert!(bus.report(&json!("tok"), 0.25, Some(1.0), Some("a".into())));
        assert!(bus.report(&json!("tok"), 0.5, Some(1.0), None));
        assert!(!bus.report(&json!("tok"), 0.5, Some(1.0), None));
        assert!(!bus.report(&json!("tok"), 0.4, Some(1.0), None));
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["method"], json!("notifications/progress"));
        assert_eq!(captured[0]["params"]["progressToken"], json!("tok"));
        assert_eq!(captured[0]["params"]["progress"], json!(0.25));
        assert_eq!(captured[0]["params"]["total"], json!(1.0));
        assert_eq!(captured[0]["params"]["message"], json!("a"));
        assert!(captured[1]["params"].get("message").is_none());
    }

    #[test]
    fn reports_progress_for_numeric_token_independently() {
        let (bus, captured) = capturing_bus();
        assert!(bus.report(&json!(1), 0.1, None, None));
        assert!(bus.report(&json!("tok"), 0.05, None, None));
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
    }

    #[test]
    fn rejects_non_finite_or_invalid_token() {
        let (bus, captured) = capturing_bus();
        assert!(!bus.report(&JsonValue::Null, 0.1, None, None));
        assert!(!bus.report(&json!(true), 0.1, None, None));
        assert!(!bus.report(&json!("tok"), f64::NAN, None, None));
        assert!(!bus.report(&json!("tok"), 0.1, Some(f64::INFINITY), None));
        assert!(captured.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn scope_context_is_visible_inside_and_absent_outside() {
        assert!(current_context().is_none());
        let (bus, _) = capturing_bus();
        let ctx = ProgressContext::new(bus, json!("tok"));
        scope_context(Some(ctx), async {
            assert!(current_context().is_some());
        })
        .await;
        assert!(current_context().is_none());
    }

    #[tokio::test]
    async fn scope_context_isolates_concurrent_tasks() {
        let (bus, captured) = capturing_bus();
        let ctx_a = ProgressContext::new(bus.clone(), json!("a"));
        let ctx_b = ProgressContext::new(bus, json!("b"));
        let task_a = scope_context(Some(ctx_a), async {
            tokio::task::yield_now().await;
            current_context().unwrap().token.clone()
        });
        let task_b = scope_context(Some(ctx_b), async {
            tokio::task::yield_now().await;
            current_context().unwrap().token.clone()
        });
        let (a, b) = tokio::join!(task_a, task_b);
        assert_eq!(a, json!("a"));
        assert_eq!(b, json!("b"));
        assert!(captured.lock().unwrap().is_empty());
    }
}
