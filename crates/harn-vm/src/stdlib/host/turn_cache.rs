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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::value::{DictMap, VmError, VmValue};

/// Monotonic turn counter, bumped by [`reset`] at every turn boundary.
///
/// Deliberately process-global rather than per-session. A turn boundary in one
/// session therefore also invalidates a concurrently-running session's memo,
/// which costs that session one extra round-trip per crossed boundary. That is
/// the conservative direction: the failure it forecloses is serving *stale*
/// turn-stable state, and the worst case degrades toward the uncached behaviour
/// this module replaced rather than toward incorrectness. Keying by session
/// would recover those hits, but `reset` is driven from an agent-loop event that
/// carries no session identity here, so it would be inferred rather than known.
///
/// The memo below is thread-local, but turn boundaries are not guaranteed to be
/// observed on the same thread that populated it — `reset` runs where the
/// agent-loop event is emitted, while a `host_call` may be served from
/// elsewhere. Storing the epoch alongside each entry makes a stale entry
/// *unreadable* rather than merely unlikely, so correctness no longer depends on
/// a reset reaching any particular thread; thread-locality is then a pure
/// performance choice. Without this, a missed reset would silently serve last
/// turn's `runtime.pipeline_input` — which is exactly the mid-session `/model`
/// switch that hosts re-project it per turn to observe.
static TURN_EPOCH: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Turn-scoped memo keyed by [`cache_key`], each entry tagged with the
    /// [`TURN_EPOCH`] it was written in. Non-authoritative and always
    /// resettable, so it is safe to keep thread-private.
    static TURN_STABLE_HOST_CACHE: RefCell<HashMap<String, (u64, VmValue)>> =
        RefCell::new(HashMap::new());
}

fn current_epoch() -> u64 {
    TURN_EPOCH.load(Ordering::Acquire)
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
    if let Some(cached) = lookup(capability, operation, params) {
        return Ok(Some(cached));
    }
    let result = dispatch()?;
    if let Some(value) = &result {
        store(capability, operation, params, value);
    }
    Ok(result)
}

/// Read a turn-stable host fact from the current turn's memo.
///
/// Returns `None` for anything not on the [`is_turn_stable`] allowlist, for a
/// cold memo, and for an entry written in an earlier turn.
///
/// Exposed because the stdlib `host_call` builtin is **not** the only
/// implementation of that builtin: an embedder can replace it wholesale (the ACP
/// adapter in `harn-serve` does, forwarding to the editor over JSON-RPC), and
/// such a replacement never reaches [`cached_or`] in the dispatch path. Those
/// implementations must front their own dispatch with this pair so every
/// `host_call` route shares one memo and one allowlist. harn#5190.
pub fn lookup(capability: &str, operation: &str, params: &DictMap) -> Option<VmValue> {
    if !is_turn_stable(capability, operation) {
        return None;
    }
    let key = cache_key(capability, operation, params);
    let epoch = current_epoch();
    TURN_STABLE_HOST_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&key)
            .filter(|(written, _)| *written == epoch)
            .map(|(_, value)| value.clone())
    })
}

/// Memoize a turn-stable host fact for the remainder of the current turn.
/// Non-allowlisted `(capability, operation)` pairs are ignored, so a caller
/// cannot widen the allowlist by calling this directly. See [`lookup`].
pub fn store(capability: &str, operation: &str, params: &DictMap, value: &VmValue) {
    if !is_turn_stable(capability, operation) {
        return;
    }
    let key = cache_key(capability, operation, params);
    let epoch = current_epoch();
    TURN_STABLE_HOST_CACHE.with(|cache| {
        cache.borrow_mut().insert(key, (epoch, value.clone()));
    });
}

/// Split a dotted `capability.operation` host-call name and [`lookup`] it.
/// Convenience for embedder `host_call` implementations, which receive the
/// dotted wire name rather than a split pair.
pub fn lookup_by_name(name: &str, params: &DictMap) -> Option<VmValue> {
    let (capability, operation) = name.split_once('.')?;
    lookup(capability, operation, params)
}

/// Dotted-name counterpart to [`store`]. See [`lookup_by_name`].
pub fn store_by_name(name: &str, params: &DictMap, value: &VmValue) {
    if let Some((capability, operation)) = name.split_once('.') {
        store(capability, operation, params, value);
    }
}

/// Open a new turn: every entry written before this call becomes unreadable.
///
/// Called at each agent-loop iteration boundary so a turn re-reads turn-stable
/// host facts exactly once, and at run/embedder boundaries so a memo can never
/// leak across runs on a reused thread. Bumping a global epoch rather than
/// clearing the thread-local map means a turn boundary observed on one thread
/// invalidates entries cached on every thread — see [`TURN_EPOCH`].
pub(crate) fn reset() {
    TURN_EPOCH.fetch_add(1, Ordering::AcqRel);
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

    /// [`TURN_EPOCH`] is process-global, so these tests mutate shared state: a
    /// `reset` in one invalidates entries another is mid-assertion about. Cargo
    /// runs them on separate threads by default, which made that a real
    /// cross-talk failure rather than a theoretical one. Serialize them.
    fn epoch_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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
        let _guard = epoch_lock().lock().unwrap_or_else(|e| e.into_inner());
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

    /// A turn boundary observed on a *different* thread must still invalidate
    /// entries cached here.
    ///
    /// This is the property that makes it safe for an embedder to front its own
    /// `host_call` with [`super::lookup`] / [`super::store`]: `reset` runs where
    /// the agent-loop event is emitted, which is not guaranteed to be the thread
    /// that populated the memo. Before epoch tagging, a reset that landed
    /// elsewhere left this thread serving the previous turn's
    /// `runtime.pipeline_input` — silently defeating the per-turn re-projection
    /// hosts rely on to observe a mid-session `/model` switch.
    #[test]
    fn turn_boundary_on_another_thread_invalidates_this_thread() {
        let _guard = epoch_lock().lock().unwrap_or_else(|e| e.into_inner());
        let params = DictMap::new();
        let cached = VmValue::String(arcstr::ArcStr::from("turn-1"));
        super::store("runtime", "pipeline_input", &params, &cached);
        assert!(
            super::lookup("runtime", "pipeline_input", &params).is_some(),
            "same-turn read must hit"
        );

        std::thread::spawn(|| reset()).join().expect("reset thread");

        assert!(
            super::lookup("runtime", "pipeline_input", &params).is_none(),
            "a turn boundary observed on another thread must invalidate this thread's entry"
        );
    }

    /// `store` cannot be used to widen the allowlist: a non-turn-stable op is
    /// dropped rather than memoized, so an embedder wiring these in cannot
    /// accidentally cache a write or a live read.
    #[test]
    fn store_ignores_non_turn_stable_operations() {
        let _guard = epoch_lock().lock().unwrap_or_else(|e| e.into_inner());
        let params = DictMap::new();
        let value = VmValue::String(arcstr::ArcStr::from("live"));
        super::store("session", "active_roots", &params, &value);
        assert!(
            super::lookup("session", "active_roots", &params).is_none(),
            "non-allowlisted reads must never be served from the memo"
        );
    }

    /// The dotted-name helpers an embedder uses must resolve to the same entry
    /// as the split-pair API, or the two `host_call` routes would keep separate
    /// memos and the ACP path would still pay every round-trip.
    #[test]
    fn dotted_name_helpers_share_the_split_pair_entry() {
        let _guard = epoch_lock().lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let params = DictMap::new();
        let value = VmValue::String(arcstr::ArcStr::from("shared"));
        super::store_by_name("runtime.pipeline_input", &params, &value);
        assert_eq!(
            super::lookup("runtime", "pipeline_input", &params).map(|v| v.display()),
            Some("shared".to_string()),
            "store_by_name must populate the entry lookup() reads"
        );
        assert_eq!(
            super::lookup_by_name("runtime.pipeline_input", &params).map(|v| v.display()),
            Some("shared".to_string()),
            "lookup_by_name must read it back"
        );
        assert!(
            super::lookup_by_name("no-separator", &params).is_none(),
            "a name without a capability separator must not panic or match"
        );
    }
}
