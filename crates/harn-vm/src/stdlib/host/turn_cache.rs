//! Per-turn memoization of turn-stable host capability reads.
//!
//! Context assembly repeatedly reads the same host facts per agent-loop
//! iteration. harn#5190 measured ~20 identical `runtime.pipeline_input`
//! round-trips per turn; Burin H-063 later measured roughly 164 repeated
//! `project.metadata_get` calls in an ordinary context build. This module
//! front-runs the thread-local `HOST_CALL_BRIDGE` with a per-turn memo so those
//! reads collapse to one host round-trip per stable fact. Project metadata is
//! fetched once as a typed directory snapshot and namespace reads project from
//! it locally. The allowlist ([`is_turn_stable`]) is deliberately
//! narrow, and metadata mutations invalidate the whole memo before and after
//! dispatch so inherited read-after-write values cannot be stale.
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

mod metadata_snapshot;

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

/// Cache semantics for a canonical host operation.
///
/// This is the single owner of both admission and invalidation: adding a
/// stable read without naming its mutators (or adding a mutator in a separate
/// string table) is therefore visible in one exhaustive match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnCacheDisposition {
    StableRead,
    Invalidates,
    Live,
}

fn disposition(capability: &str, operation: &str) -> TurnCacheDisposition {
    match (capability, operation) {
        ("runtime", "pipeline_input") | ("project", "metadata_get") => {
            TurnCacheDisposition::StableRead
        }
        ("project", "metadata_set" | "metadata_save" | "metadata_refresh_hashes") => {
            TurnCacheDisposition::Invalidates
        }
        _ => TurnCacheDisposition::Live,
    }
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
/// The qualifying reads are:
/// - `runtime.pipeline_input`: every Burin host recomputes it per turn from
///   per-turn-stable inputs (model selection, task, dry-run), so a mid-turn
///   re-read never diverges.
/// - `project.metadata_get`: directory metadata is coherent for a turn unless
///   the canonical metadata mutation operations change it. Those writes
///   invalidate the global epoch on both sides of dispatch. Directory keys stay
///   distinct while sibling namespaces share one validated bulk snapshot.
///
/// Deliberately excluded after auditing the producers:
/// - `session.active_roots` — the IDE host serves it live from the mutable
///   workspace root set (also used live for path validation), so a user adding
///   a root mid-turn would be served stale within the turn.
/// - `project.metadata_inspect` / `metadata_stale` — their freshness fields can
///   change when workspace files change, independently of metadata mutations.
/// - `runtime.task` / `runtime.dry_run` / `runtime.approved_plan` — not served
///   as standalone host ops by the Burin hosts at all; their values ride inside
///   `pipeline_input` (already cached here), so caching the standalone op buys
///   nothing.
fn is_turn_stable(capability: &str, operation: &str) -> bool {
    disposition(capability, operation) == TurnCacheDisposition::StableRead
}

/// True when a host operation can change a memoized fact.
///
/// Metadata resolution is hierarchical: writing an ancestor can change a
/// descendant read, and saving can make host-managed metadata visible through
/// a different backend. Exact-key eviction would therefore be unsound. The
/// caller opens a fresh global epoch both before and after these operations,
/// which also makes concurrent cross-thread refills conservative rather than
/// stale. Writes are rare, so invalidating `runtime.pipeline_input` alongside
/// metadata is a cheaper and safer seam than a second metadata-only epoch.
fn invalidates_turn_stable_reads(capability: &str, operation: &str) -> bool {
    disposition(capability, operation) == TurnCacheDisposition::Invalidates
}

/// Invalidates turn-stable reads around a host mutation, including early
/// returns and errors from the canonical dispatcher.
pub(crate) struct InvalidationScope {
    invalidates: bool,
}

impl Drop for InvalidationScope {
    fn drop(&mut self) {
        if self.invalidates {
            reset();
        }
    }
}

pub(crate) fn invalidation_scope(capability: &str, operation: &str) -> InvalidationScope {
    let invalidates = invalidates_turn_stable_reads(capability, operation);
    if invalidates {
        reset();
    }
    InvalidationScope { invalidates }
}

/// Cache key for a turn-stable host call. Keyed on capability, operation, and a
/// canonical fingerprint of the params so `project.metadata_get` caches per
/// distinct argument set while no-arg reads retain the cheap fast path.
/// Dynamic object insertion order is not semantic, so the fingerprint uses
/// the runtime's canonical JSON owner.
fn cache_key(capability: &str, operation: &str, params: &DictMap) -> String {
    if params.is_empty() {
        return format!("{capability}.{operation}");
    }
    let json = crate::llm::helpers::vm_value_to_json(&VmValue::dict(params.clone()));
    format!(
        "{capability}.{operation}#{}",
        crate::canonical_json::to_string(&json)
    )
}

/// Serve `(capability, operation, params)` from the per-turn memo when it is a
/// turn-stable read, otherwise run `dispatch` verbatim. A successful
/// `Ok(Some(value))` from a turn-stable read is memoized for the rest of the
/// turn; `Ok(None)` (the bridge declined) and errors are never cached.
pub(crate) async fn cached_or<F, Fut>(
    capability: &str,
    operation: &str,
    params: &DictMap,
    dispatch: F,
) -> Result<Option<VmValue>, VmError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Option<VmValue>, VmError>>,
{
    if !is_turn_stable(capability, operation) {
        return dispatch().await;
    }
    if let Some(cached) = lookup(capability, operation, params) {
        return Ok(Some(cached));
    }
    let dispatch_epoch = current_epoch();
    let result = dispatch().await?;
    if let Some(value) = &result {
        store_at_epoch(capability, operation, params, value, dispatch_epoch);
    }
    Ok(result)
}

/// Metadata-specific deep cache interface: validate one directory snapshot,
/// project namespaces locally, and coalesce concurrent cold reads. The
/// dispatcher receives owned canonical params because a namespace request is
/// deliberately replaced with one namespace-free bulk request.
pub(crate) async fn cached_metadata_or<F, Fut>(
    params: &DictMap,
    dispatch: F,
) -> Result<Option<VmValue>, VmError>
where
    F: FnOnce(DictMap) -> Fut,
    Fut: std::future::Future<Output = Result<Option<VmValue>, VmError>>,
{
    metadata_snapshot::cached_or(params, current_epoch(), dispatch).await
}

/// Read a turn-stable host fact from the current turn's memo.
///
/// Returns `None` for anything not on the [`is_turn_stable`] allowlist, for a
/// cold memo, and for an entry written in an earlier turn.
///
/// Read API for the turn memo. Prefer going through canonical
/// [`super::dispatch_host_operation`]; this exists for tests that seed or
/// inspect the memo directly (harn#5190 / harn#5523).
pub fn lookup(capability: &str, operation: &str, params: &DictMap) -> Option<VmValue> {
    if !is_turn_stable(capability, operation) {
        return None;
    }
    let epoch = current_epoch();
    if capability == "project" && operation == "metadata_get" {
        return metadata_snapshot::lookup(params, epoch);
    }
    let key = cache_key(capability, operation, params);
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
    store_at_epoch(capability, operation, params, value, current_epoch());
}

/// Store a value only in the epoch in which its host dispatch began.
///
/// A read can be in flight while a metadata mutation opens a new epoch. It may
/// still return its result to that original caller, but tagging the memo entry
/// with the captured epoch makes the stale refill unreadable. Loading the
/// current epoch and then storing with it would let a slow pre-write read poison
/// the post-write cache after the mutator's trailing reset.
fn store_at_epoch(
    capability: &str,
    operation: &str,
    params: &DictMap,
    value: &VmValue,
    dispatch_epoch: u64,
) {
    if !is_turn_stable(capability, operation) {
        return;
    }
    if current_epoch() != dispatch_epoch {
        return;
    }
    if capability == "project" && operation == "metadata_get" {
        metadata_snapshot::store(params, value, dispatch_epoch);
        return;
    }
    let key = cache_key(capability, operation, params);
    TURN_STABLE_HOST_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .insert(key, (dispatch_epoch, value.clone()));
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

/// Open a new turn: every entry written before this call becomes unreadable,
/// on this thread and every other.
///
/// Called at each agent-loop iteration boundary so a turn re-reads turn-stable
/// host facts exactly once, and at bridge install/teardown so a memo can never
/// leak across embedders on a reused thread. Bumping a global epoch rather than
/// clearing the thread-local map means a turn boundary observed on one thread
/// invalidates entries cached on every thread — see [`TURN_EPOCH`].
pub(crate) fn reset() {
    TURN_EPOCH.fetch_add(1, Ordering::AcqRel);
    reset_local();
}

/// Drop this thread's entries without opening a new turn.
///
/// For `reset_host_state`, reached from `reset_stdlib_state` and in turn from
/// [`crate::reset_thread_local_state`] — whose contract is to reset *this
/// thread*, and which runs between VM runs on a reused thread rather than at any
/// turn boundary. [`TURN_EPOCH`] is process-global, so bumping it from there
/// reaches past that contract: every VM run that ended anywhere would invalidate
/// the live memo of every concurrently-running session, costing each an extra
/// round-trip. A thread-local reset should clear thread-local state only.
///
/// This is exactly the pre-epoch behaviour of [`reset`], so the call sites moved
/// here keep the semantics they already had; only genuine turn boundaries and
/// bridge swaps gained cross-thread reach.
pub(crate) fn reset_local() {
    TURN_STABLE_HOST_CACHE.with(|cache| cache.borrow_mut().clear());
    metadata_snapshot::reset_local();
}

/// Serializes tests that bump [`TURN_EPOCH`] against tests that rely on a memo
/// entry surviving between a `store` and a `lookup`.
///
/// The epoch is process-global, so without this a bridge swap in one test
/// invalidates another test's entry mid-assertion. Mirrors the
/// `LONG_RUNNING_TEST_LOCK` convention in `stdlib::fs::tests` for the same
/// reason: process-global state needs process-global test exclusion.
#[cfg(test)]
pub(crate) fn epoch_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::{
        clear_host_call_bridge, dispatch_host_operation, reset_host_state, set_host_call_bridge,
        HostCallBridge,
    };
    use super::reset;
    use crate::value::{DictMap, VmValue};

    /// [`TURN_EPOCH`] is process-global, so these tests mutate shared state: a
    /// `reset` in one invalidates entries another is mid-assertion about. Cargo
    /// runs them on separate threads by default, which made that a real
    /// cross-talk failure rather than a theoretical one. Serialize them.
    /// Bridge that counts dispatches per `(capability, operation)` and answers
    /// every op, so a test can assert how many times the host was actually hit.
    struct CountingRuntimeBridge {
        counts: Arc<Mutex<std::collections::HashMap<(String, String), usize>>>,
    }

    struct VersionedMetadataBridge {
        generation: Arc<std::sync::atomic::AtomicUsize>,
        counts: Arc<Mutex<std::collections::HashMap<(String, String), usize>>>,
    }

    impl HostCallBridge for VersionedMetadataBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            params: &'a DictMap,
        ) -> super::super::HostCallDispatchFuture<'a> {
            *self
                .counts
                .lock()
                .unwrap()
                .entry((capability.to_string(), operation.to_string()))
                .or_insert(0) += 1;
            if capability == "project" && operation == "metadata_set" {
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return super::super::host_call_ready(Ok(Some(VmValue::Nil)));
            }
            let generation = self.generation.load(std::sync::atomic::Ordering::SeqCst);
            assert!(
                params.get("namespace").is_none(),
                "metadata snapshots must be fetched without a namespace projection"
            );
            let namespace = |name: &str| {
                (
                    crate::value::intern_key(name),
                    VmValue::dict(DictMap::from_iter([(
                        crate::value::intern_key("generation"),
                        VmValue::Int(generation as i64),
                    )])),
                )
            };
            super::super::host_call_ready(Ok(Some(VmValue::dict(DictMap::from_iter([
                namespace("facts"),
                namespace("test"),
            ])))))
        }
    }

    impl HostCallBridge for CountingRuntimeBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            _params: &'a DictMap,
        ) -> super::super::HostCallDispatchFuture<'a> {
            *self
                .counts
                .lock()
                .unwrap()
                .entry((capability.to_string(), operation.to_string()))
                .or_insert(0) += 1;
            super::super::host_call_ready(Ok(Some(VmValue::String(arcstr::ArcStr::from(format!(
                "{capability}.{operation}"
            ))))))
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
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn metadata_namespaces_share_a_snapshot_and_writes_invalidate_inherited_values() {
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        run_async(|| async {
            reset_host_state();
            let counts = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let generation = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            set_host_call_bridge(Arc::new(VersionedMetadataBridge {
                generation,
                counts: counts.clone(),
            }));

            let count = |op: &str| -> usize {
                counts
                    .lock()
                    .unwrap()
                    .get(&("project".to_string(), op.to_string()))
                    .copied()
                    .unwrap_or(0)
            };
            let descendant_facts = DictMap::from_iter([
                (
                    crate::value::intern_key("dir"),
                    VmValue::String(arcstr::ArcStr::from("src/nested")),
                ),
                (
                    crate::value::intern_key("namespace"),
                    VmValue::String(arcstr::ArcStr::from("facts")),
                ),
            ]);
            let descendant_test = DictMap::from_iter([
                (
                    crate::value::intern_key("dir"),
                    VmValue::String(arcstr::ArcStr::from("src/nested")),
                ),
                (
                    crate::value::intern_key("namespace"),
                    VmValue::String(arcstr::ArcStr::from("test")),
                ),
            ]);

            for _ in 0..100 {
                let value = dispatch_host_operation("project", "metadata_get", &descendant_facts)
                    .await
                    .expect("metadata_get");
                assert!(matches!(
                    value.as_dict().and_then(|fields| fields.get("generation")),
                    Some(VmValue::Int(0))
                ));
            }
            assert_eq!(
                count("metadata_get"),
                1,
                "100 exact reads must dispatch once"
            );

            dispatch_host_operation("project", "metadata_get", &descendant_test)
                .await
                .expect("parameter-distinct metadata_get");
            assert_eq!(
                count("metadata_get"),
                1,
                "sibling namespaces must project from one directory snapshot"
            );

            let ancestor_write = DictMap::from_iter([
                (
                    crate::value::intern_key("dir"),
                    VmValue::String(arcstr::ArcStr::from("src")),
                ),
                (
                    crate::value::intern_key("namespace"),
                    VmValue::String(arcstr::ArcStr::from("facts")),
                ),
                (
                    crate::value::intern_key("value"),
                    VmValue::dict(DictMap::new()),
                ),
            ]);
            dispatch_host_operation("project", "metadata_set", &ancestor_write)
                .await
                .expect("metadata_set");
            assert_eq!(count("metadata_set"), 1);

            let refreshed = dispatch_host_operation("project", "metadata_get", &descendant_facts)
                .await
                .expect("read after ancestor write");
            assert!(
                matches!(
                    refreshed
                        .as_dict()
                        .and_then(|fields| fields.get("generation")),
                    Some(VmValue::Int(1))
                ),
                "an ancestor write must invalidate a cached descendant read"
            );
            assert_eq!(count("metadata_get"), 2);

            reset();
            dispatch_host_operation("project", "metadata_get", &descendant_facts)
                .await
                .expect("next-turn metadata_get");
            assert_eq!(count("metadata_get"), 3, "the next turn must re-read once");

            clear_host_call_bridge();
        });
    }

    #[test]
    fn every_canonical_metadata_mutator_invalidates_turn_stable_reads() {
        for operation in ["metadata_set", "metadata_save", "metadata_refresh_hashes"] {
            assert!(
                super::invalidates_turn_stable_reads("project", operation),
                "project.{operation} must invalidate the metadata read memo"
            );
        }
        for operation in ["metadata_get", "metadata_inspect", "metadata_stale"] {
            assert!(
                !super::invalidates_turn_stable_reads("project", operation),
                "read-only project.{operation} must not open a new epoch"
            );
        }
    }

    #[test]
    fn mutation_scope_invalidates_before_and_after_every_return_path() {
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let before = super::current_epoch();
        {
            let _scope = super::invalidation_scope("project", "metadata_set");
            assert!(
                super::current_epoch() > before,
                "mutation must invalidate before dispatch"
            );
        }
        let after_mutation = super::current_epoch();
        assert!(
            after_mutation > before + 1,
            "scope drop must invalidate after dispatch"
        );

        {
            let _scope = super::invalidation_scope("project", "metadata_get");
        }
        assert_eq!(
            super::current_epoch(),
            after_mutation,
            "read-only dispatch must not invalidate the memo"
        );
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
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let params = DictMap::new();
        let cached = VmValue::String(arcstr::ArcStr::from("turn-1"));
        super::store("runtime", "pipeline_input", &params, &cached);
        assert!(
            super::lookup("runtime", "pipeline_input", &params).is_some(),
            "same-turn read must hit"
        );

        std::thread::spawn(reset).join().expect("reset thread");

        assert!(
            super::lookup("runtime", "pipeline_input", &params).is_none(),
            "a turn boundary observed on another thread must invalidate this thread's entry"
        );
    }

    /// A host read that began before a mutation may complete after the
    /// mutator's trailing reset. The result is valid for its original caller,
    /// but it must not refill the new epoch's memo.
    #[test]
    fn pre_mutation_read_cannot_poison_the_post_mutation_epoch() {
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset();
        let params = DictMap::from_iter([(
            crate::value::intern_key("dir"),
            VmValue::String(arcstr::ArcStr::from("src")),
        )]);
        let dispatch_epoch = super::current_epoch();

        // Models a metadata write completing while this read is in flight.
        reset();
        super::store_at_epoch(
            "project",
            "metadata_get",
            &params,
            &VmValue::dict(DictMap::from_iter([(
                crate::value::intern_key("facts"),
                VmValue::dict(DictMap::new()),
            )])),
            dispatch_epoch,
        );

        assert!(
            super::lookup("project", "metadata_get", &params).is_none(),
            "an old dispatch result must not become the new epoch's cached value"
        );
    }

    /// `store` cannot be used to widen the allowlist: a non-turn-stable op is
    /// dropped rather than memoized, so an embedder wiring these in cannot
    /// accidentally cache a write or a live read.
    #[test]
    fn store_ignores_non_turn_stable_operations() {
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        let _guard = super::epoch_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
