//! Per-turn memoization of turn-stable host capability reads.
//!
//! Context assembly reads `runtime.pipeline_input` many times per agent-loop
//! iteration — harn#5190 measured ~20 identical round-trips per turn, a cost
//! that grows as hosts deliver more data through that channel. This module
//! front-runs the thread-local `HOST_CALL_BRIDGE` with a per-turn memo so those
//! reads collapse to one host round-trip per turn, leaving every call site
//! unchanged. The allowlist ([`is_turn_stable`]) is deliberately narrow.
//!
//! The memoized value is stable only *within* a turn: the host re-projects
//! `runtime.pipeline_input` each turn (e.g. so a mid-session model switch is
//! observed on the next prompt). The memo is therefore cleared at each
//! agent-loop iteration boundary (`iteration_start`, wired in
//! `__host_agent_emit_event`) and at run/embedder boundaries via [`reset`].

use std::cell::RefCell;
use std::collections::HashMap;

use crate::value::{DictMap, VmError, VmValue};

thread_local! {
    /// Turn-scoped memo keyed by [`cache_key`]. Non-authoritative and always
    /// resettable, so it is safe to keep thread-private: it is co-located with
    /// the thread-local `HOST_CALL_BRIDGE` it front-runs and never carries
    /// state a work-stealing task must observe.
    static TURN_STABLE_HOST_CACHE: RefCell<HashMap<String, VmValue>> =
        RefCell::new(HashMap::new());
}

/// True for host capabilities whose result is stable for the duration of a
/// single agent-loop iteration: pure, side-effect-free reads that project the
/// current turn's host input.
///
/// Membership is an explicit allowlist, and the bar for adding an entry is a
/// producer-side citation that no host mutates the value *within* a turn — not
/// merely that it "looks stable." The default is NOT to cache, so a write, an
/// interactive prompt, or any value a host can change mid-turn is never served
/// stale.
///
/// Only `runtime.pipeline_input` qualifies today: every Burin host recomputes it
/// per turn from per-turn-stable inputs (model selection, task, dry-run), so a
/// mid-turn re-read never diverges. Deliberately excluded after auditing the
/// producers:
/// - `session.active_roots` — the IDE host serves it live from the mutable
///   workspace root set (also used live for path validation), so a user adding
///   a root mid-turn would be served stale within the turn.
/// - `runtime.task` / `runtime.dry_run` / `runtime.approved_plan` — not served
///   as standalone host ops by the Burin hosts at all; their values ride inside
///   `pipeline_input` (already cached here), so caching the standalone op buys
///   nothing.
fn is_turn_stable(capability: &str, operation: &str) -> bool {
    matches!((capability, operation), ("runtime", "pipeline_input"))
}

/// Cache key for a turn-stable host call. Keyed on capability, operation, and a
/// canonical fingerprint of the params so a (future) parameterized read caches
/// per distinct argument set; the current allowlist is all no-arg reads that
/// take the cheap empty-params path. serde_json's `Map` is key-sorted here (no
/// `preserve_order` feature), so the fingerprint is stable.
fn cache_key(capability: &str, operation: &str, params: &DictMap) -> String {
    if params.is_empty() {
        return format!("{capability}.{operation}");
    }
    let json = crate::llm::helpers::vm_value_to_json(&VmValue::dict(params.clone()));
    format!(
        "{capability}.{operation}#{}",
        serde_json::to_string(&json).unwrap_or_default()
    )
}

/// Serve `(capability, operation, params)` from the per-turn memo when it is a
/// turn-stable read, otherwise run `dispatch` verbatim. A successful
/// `Ok(Some(value))` from a turn-stable read is memoized for the rest of the
/// turn; `Ok(None)` (the bridge declined) and errors are never cached.
pub(crate) fn cached_or<F>(
    capability: &str,
    operation: &str,
    params: &DictMap,
    dispatch: F,
) -> Result<Option<VmValue>, VmError>
where
    F: FnOnce() -> Result<Option<VmValue>, VmError>,
{
    if !is_turn_stable(capability, operation) {
        return dispatch();
    }
    let key = cache_key(capability, operation, params);
    if let Some(cached) = TURN_STABLE_HOST_CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
        return Ok(Some(cached));
    }
    let result = dispatch()?;
    if let Some(value) = &result {
        TURN_STABLE_HOST_CACHE.with(|cache| {
            cache.borrow_mut().insert(key, value.clone());
        });
    }
    Ok(result)
}

/// Clear the per-turn host-capability memo. Called at each agent-loop iteration
/// boundary so a turn re-reads turn-stable host facts exactly once, and at
/// run/embedder boundaries so a memo can never leak across runs on a reused
/// thread.
pub(crate) fn reset() {
    TURN_STABLE_HOST_CACHE.with(|cache| cache.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{
        clear_host_call_bridge, dispatch_host_operation, reset_host_state, set_host_call_bridge,
        HostCallBridge,
    };
    use super::reset;
    use crate::value::{DictMap, VmError, VmValue};

    /// Bridge that counts dispatches per `(capability, operation)` and answers
    /// every op, so a test can assert how many times the host was actually hit.
    struct CountingRuntimeBridge {
        counts: Arc<Mutex<std::collections::HashMap<(String, String), usize>>>,
    }

    impl HostCallBridge for CountingRuntimeBridge {
        fn dispatch(
            &self,
            capability: &str,
            operation: &str,
            _params: &DictMap,
        ) -> Result<Option<VmValue>, VmError> {
            *self
                .counts
                .lock()
                .unwrap()
                .entry((capability.to_string(), operation.to_string()))
                .or_insert(0) += 1;
            Ok(Some(VmValue::String(arcstr::ArcStr::from(format!(
                "{capability}.{operation}"
            )))))
        }
    }

    fn run_async<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local.run_until(test()).await;
        });
    }

    #[test]
    fn turn_stable_host_capability_is_fetched_once_per_turn() {
        run_async(|| async {
            reset_host_state();
            let counts = Arc::new(Mutex::new(std::collections::HashMap::new()));
            set_host_call_bridge(Arc::new(CountingRuntimeBridge {
                counts: counts.clone(),
            }));

            let count = |cap: &str, op: &str| -> usize {
                counts
                    .lock()
                    .unwrap()
                    .get(&(cap.to_string(), op.to_string()))
                    .copied()
                    .unwrap_or(0)
            };

            // Many reads within one turn collapse to a single host round-trip.
            for _ in 0..20 {
                dispatch_host_operation("runtime", "pipeline_input", &DictMap::new())
                    .await
                    .expect("pipeline_input");
            }
            assert_eq!(
                count("runtime", "pipeline_input"),
                1,
                "20 same-turn reads must hit the host exactly once"
            );

            // A non-allowlisted op is never memoized: every call reaches the host.
            for _ in 0..3 {
                dispatch_host_operation("runtime", "record_run", &DictMap::new())
                    .await
                    .expect("record_run");
            }
            assert_eq!(
                count("runtime", "record_run"),
                3,
                "writes/non-stable ops must never be served from the turn memo"
            );

            // The next turn boundary re-reads the turn-stable fact exactly once,
            // so a mid-session change (e.g. a model switch) is observed.
            reset();
            for _ in 0..20 {
                dispatch_host_operation("runtime", "pipeline_input", &DictMap::new())
                    .await
                    .expect("pipeline_input");
            }
            assert_eq!(
                count("runtime", "pipeline_input"),
                2,
                "a new turn must re-fetch once, not serve the prior turn's value"
            );

            clear_host_call_bridge();
        });
    }
}
