//! Non-blocking session warm for the code index.
//!
//! Embedders call [`CodeIndexCapability::warm_session`] at session start so a
//! workspace can restore a snapshot or rebuild in the background without
//! stalling the model's first turn. Both paths run on the warm thread: a stale
//! snapshot's reconcile can cost as much as a cold build on a large repo. Sync
//! [`hostlib_code_index_rebuild`] joins the same single-flight gate so
//! `ensure_initialised` does not start a second full walk while the warm is
//! still running. Once the slot is live, a later
//! sync rebuild still re-walks disk — join only applies to an in-flight build.
//! A sync rebuild into an empty slot restores the root's snapshot first, the
//! same way the warm does, and walks the whole tree only when none is usable.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::VmValue;

use super::builtins::SharedIndex;
use super::state::{canonicalize, IndexState};
use super::CodeIndexCapability;
use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_bool, optional_string, str_value};
use crate::HarnReferenceResolver;

/// Outcome of [`CodeIndexCapability::warm_session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionWarmOutcome {
    /// In-memory index was already populated.
    AlreadyLive,
    /// A background restore or rebuild is in flight (started now or already
    /// running).
    Building,
    /// The background thread could not be spawned; callers may sync-rebuild.
    SpawnFailed,
}

#[derive(Debug)]
struct WarmState {
    /// Canonical root of the in-flight build, when any.
    in_flight_root: Option<PathBuf>,
    /// Generation bumped whenever an in-flight build finishes.
    generation: u64,
    /// Snapshot writes scheduled after a restore published its index and not
    /// yet finished. `wait_until_idle` waits for these as well as the flight.
    pending_persists: usize,
}

impl WarmState {
    /// No build in flight and no snapshot write outstanding.
    fn is_idle(&self) -> bool {
        self.in_flight_root.is_none() && self.pending_persists == 0
    }
}

/// How often a waiter emits progress while an in-flight build holds the gate.
const WAIT_HEARTBEAT: Duration = Duration::from_secs(1);

/// Single-flight coordinator shared by session warm and sync rebuild.
#[derive(Debug)]
pub(super) struct WarmCoordinator {
    state: Mutex<WarmState>,
    cv: Condvar,
    builder: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Default for WarmCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(WarmState {
                in_flight_root: None,
                generation: 0,
                pending_persists: 0,
            }),
            cv: Condvar::new(),
            builder: Mutex::new(None),
        }
    }
}

/// RAII guard for one scheduled snapshot write; wakes waiters on drop.
struct PendingPersist {
    warm: Arc<WarmCoordinator>,
}

impl Drop for PendingPersist {
    fn drop(&mut self) {
        let mut state = self.warm.state.lock().expect("warm coordinator poisoned");
        state.pending_persists -= 1;
        drop(state);
        self.warm.cv.notify_all();
    }
}

/// RAII guard that clears the in-flight warm marker on drop (including panic).
pub(super) struct WarmFlight {
    warm: Arc<WarmCoordinator>,
    root: PathBuf,
}

impl Drop for WarmFlight {
    fn drop(&mut self) {
        self.warm.end(&self.root);
    }
}

impl WarmCoordinator {
    fn wait_if_building(&self, root: &Path) {
        let canonical = canonicalize(root);
        let mut guard = self.state.lock().expect("warm coordinator poisoned");
        guard = self.wait_while(guard, &canonical, |state| {
            state
                .in_flight_root
                .as_ref()
                .is_some_and(|inflight| inflight == &canonical)
        });
        drop(guard);
    }

    pub(super) fn is_idle(&self) -> bool {
        self.state
            .lock()
            .expect("warm coordinator poisoned")
            .is_idle()
    }

    pub(super) fn wait_until_idle(&self) {
        let mut guard = self.state.lock().expect("warm coordinator poisoned");
        let root = guard
            .in_flight_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        guard = self.wait_while(guard, &root, |state| !state.is_idle());
        drop(guard);
    }

    pub(super) fn take_and_join_builder(&self) {
        let handle = self
            .builder
            .lock()
            .expect("warm coordinator poisoned")
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Count a snapshot write that `wait_until_idle` must wait for. The
    /// count drops when the returned guard does, including on panic.
    fn begin_persist(self: &Arc<Self>) -> PendingPersist {
        self.state
            .lock()
            .expect("warm coordinator poisoned")
            .pending_persists += 1;
        PendingPersist { warm: self.clone() }
    }

    /// Rewrite the snapshot off the caller's thread.
    pub(super) fn persist_in_background(self: &Arc<Self>, index: SharedIndex) {
        let pending = self.begin_persist();
        let spawned = harn_parser::runtime_stack::builder()
            .name("harn-code-index-persist".to_string())
            .spawn(move || {
                let _pending = pending;
                super::persist_reconciled(&index);
            });
        if let Err(error) = spawned {
            tracing::debug!(
                target: "harn_hostlib::code_index",
                %error,
                "code-index persist thread could not start; snapshot left as is",
            );
        }
    }

    fn store_builder(&self, handle: thread::JoinHandle<()>) {
        let mut slot = self.builder.lock().expect("warm coordinator poisoned");
        *slot = Some(handle);
    }

    fn wait_while<'a>(
        &'a self,
        mut guard: std::sync::MutexGuard<'a, WarmState>,
        root: &Path,
        mut still_waiting: impl FnMut(&WarmState) -> bool,
    ) -> std::sync::MutexGuard<'a, WarmState> {
        let started = Instant::now();
        while still_waiting(&guard) {
            #[cfg(test)]
            super::persistence_tests::waiting(self, guard.pending_persists);
            let (next, wait) = self
                .cv
                .wait_timeout(guard, WAIT_HEARTBEAT)
                .expect("warm coordinator poisoned");
            guard = next;
            if wait.timed_out() && still_waiting(&guard) {
                emit_wait_progress(root, started.elapsed());
            }
        }
        guard
    }

    /// Reserve an idle coordinator without blocking the session-start caller.
    fn try_begin(self: &Arc<Self>, root: &Path) -> Option<WarmFlight> {
        let canonical = canonicalize(root);
        let guard = self.state.lock().expect("warm coordinator poisoned");
        if !guard.is_idle() {
            return None;
        }
        Some(self.reserve(guard, canonical))
    }

    /// Join a same-root builder; otherwise wait to own the next rebuild.
    fn join_or_begin(self: &Arc<Self>, root: &Path) -> Option<WarmFlight> {
        let canonical = canonicalize(root);
        let guard = self.state.lock().expect("warm coordinator poisoned");
        if guard
            .in_flight_root
            .as_ref()
            .is_some_and(|inflight| inflight == &canonical)
        {
            return None;
        }
        Some(self.begin_after_idle(guard, canonical))
    }

    /// Own the next restore or explicit write after earlier work finishes.
    /// Readers can use the published index while this admission waits.
    pub(super) fn begin(self: &Arc<Self>, root: &Path) -> WarmFlight {
        let canonical = canonicalize(root);
        let guard = self.state.lock().expect("warm coordinator poisoned");
        self.begin_after_idle(guard, canonical)
    }

    fn begin_after_idle<'a>(
        self: &'a Arc<Self>,
        guard: std::sync::MutexGuard<'a, WarmState>,
        canonical: PathBuf,
    ) -> WarmFlight {
        // A restore writes checkpoints before publishing its private state.
        // Drain older writes before it can create a newer checkpoint.
        let guard = self.wait_while(guard, &canonical, |state| !state.is_idle());
        self.reserve(guard, canonical)
    }

    fn reserve(
        self: &Arc<Self>,
        mut guard: std::sync::MutexGuard<'_, WarmState>,
        canonical: PathBuf,
    ) -> WarmFlight {
        guard.in_flight_root = Some(canonical.clone());
        WarmFlight {
            warm: Arc::clone(self),
            root: canonical,
        }
    }

    fn end(&self, root: &Path) {
        let canonical = canonicalize(root);
        let mut guard = self.state.lock().expect("warm coordinator poisoned");
        if guard.in_flight_root.as_ref() == Some(&canonical) {
            guard.in_flight_root = None;
            guard.generation = guard.generation.wrapping_add(1);
            self.cv.notify_all();
        }
    }
}

fn emit_wait_progress(root: &Path, waited: Duration) {
    let waited_ms = waited.as_millis() as u64;
    tracing::info!(
        target: "harn_hostlib::code_index",
        root = %root.display(),
        waited_ms,
        "code-index rebuild waiting on in-flight build",
    );
    let Some(session_id) = harn_vm::agent_sessions::current_session_id() else {
        return;
    };
    let payload = serde_json::json!({
        "event": "code_index_rebuild_wait",
        "root": root.display().to_string(),
        "waited_ms": waited_ms,
    });
    harn_vm::orchestration::agent_inbox::push(
        &session_id,
        "tool_progress",
        &payload.to_string(),
        "hostlib.code_index.rebuild",
    );
}

impl CodeIndexCapability {
    /// Warm the shared index for `workspace_root` without blocking the
    /// caller on disk or parse work.
    ///
    /// If the in-memory slot is already populated, returns
    /// [`SessionWarmOutcome::AlreadyLive`]. Otherwise starts (or joins) a
    /// single-flight background flight that tries [`Self::restore_from_disk`]
    /// and, when no usable snapshot exists, runs
    /// [`IndexState::build_from_root`], installs it into the shared slot, and
    /// persists it. Restore stays off the caller's thread because reconciling
    /// a stale snapshot refreshes every changed file and rebuilds its call
    /// graph, which takes minutes on a large repo (harn#9272).
    ///
    /// Sync `hostlib_code_index_rebuild` joins the same gate, so a reader that
    /// calls `ensure_initialised` while the warm is running waits for the
    /// in-flight build instead of starting a second walk.
    pub fn warm_session(&self, workspace_root: impl AsRef<Path>) -> SessionWarmOutcome {
        let root = canonicalize(workspace_root.as_ref());
        {
            let guard = self.index.lock().expect("code_index mutex poisoned");
            if guard.is_some() {
                return SessionWarmOutcome::AlreadyLive;
            }
        }

        let Some(flight) = self.warm.try_begin(&root) else {
            return SessionWarmOutcome::Building;
        };

        let index = self.index.clone();
        let resolver = self.harn_reference_resolver.clone();
        let thread_root = root.clone();
        let warm = self.warm.clone();
        match harn_parser::runtime_stack::builder()
            .name("harn-code-index-warm".to_string())
            .spawn(move || {
                let flight = flight;
                lower_to_background_priority();
                let started = Instant::now();
                match super::restore_shared(&index, resolver.as_ref(), &thread_root) {
                    Ok(super::Restored::Published { needs_persist, .. }) => {
                        tracing::info!(
                            target: "harn_hostlib::code_index",
                            root = %thread_root.display(),
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "code-index background restore complete",
                        );
                        // Readers waiting on the flight need the published
                        // index, not the snapshot write. Count the write
                        // before ending the flight so no waiter misses it.
                        let pending = needs_persist.then(|| warm.begin_persist());
                        drop(flight);
                        if pending.is_some() {
                            super::persist_reconciled(&index);
                        }
                        return;
                    }
                    Ok(super::Restored::Absent) => {}
                    Err(error) => {
                        tracing::debug!(
                            target: "harn_hostlib::code_index",
                            %error,
                            root = %thread_root.display(),
                            "code-index snapshot restore failed; falling back to rebuild",
                        );
                    }
                }
                let (mut state, outcome) = IndexState::build_from_root(&thread_root);
                state.relink_harn_references(resolver.as_ref());
                {
                    let mut guard = index.lock().expect("code_index mutex poisoned");
                    // Prefer an already-installed index (e.g. a finished sync
                    // rebuild that raced us after we began) over clobbering.
                    if guard.is_none() {
                        *guard = Some(state);
                    }
                }
                if let Err(error) = super::persist_shared(&index) {
                    tracing::debug!(
                        target: "harn_hostlib::code_index",
                        %error,
                        root = %thread_root.display(),
                        "code-index warm persist failed",
                    );
                }
                tracing::info!(
                    target: "harn_hostlib::code_index",
                    root = %thread_root.display(),
                    files_indexed = outcome.files_indexed,
                    files_skipped = outcome.files_skipped,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "code-index background warm complete",
                );
            }) {
            Ok(handle) => {
                self.warm.store_builder(handle);
                SessionWarmOutcome::Building
            }
            Err(error) => {
                // `spawn` drops the closure (and thus `flight`) on failure.
                tracing::debug!(
                    target: "harn_hostlib::code_index",
                    %error,
                    root = %root.display(),
                    "code-index background warm spawn failed",
                );
                SessionWarmOutcome::SpawnFailed
            }
        }
    }
}

/// Run the rest of the calling thread at background priority.
///
/// The warm build is throughput work no caller waits on, and at normal
/// priority it competed for the CPU with the engine thread that is preparing
/// the session's first model call. Best effort: a refusal leaves the thread at
/// its current priority.
fn lower_to_background_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets the QoS class of the calling thread only; no memory is read
    // or written through the call.
    unsafe {
        let _ = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: on Linux a thread id names exactly the calling thread, so this
    // raises only its own nice value.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid, BACKGROUND_NICE);
    }
}

/// Nice value for the warm thread on Linux: below every interactive thread,
/// above nothing a user would notice.
#[cfg(target_os = "linux")]
const BACKGROUND_NICE: libc::c_int = 10;

fn live_stats_for_root(
    index: &SharedIndex,
    canonical: &Path,
    elapsed: Duration,
) -> Option<VmValue> {
    let guard = index.lock().expect("code_index mutex poisoned");
    let state = guard.as_ref()?;
    if state.root != *canonical {
        return None;
    }
    Some(build_dict([
        // Joined another builder's flight: this call did no work itself,
        // and says so rather than reporting the other flight's counts.
        ("mode", str_value("refreshed")),
        ("files_indexed", VmValue::Int(state.files.len() as i64)),
        ("files_skipped", VmValue::Int(0)),
        ("files_reindexed", VmValue::Int(0)),
        ("files_unchanged", VmValue::Int(state.files.len() as i64)),
        ("files_added", VmValue::Int(0)),
        ("files_removed", VmValue::Int(0)),
        ("elapsed_ms", VmValue::Int(elapsed.as_millis() as i64)),
    ]))
}

/// Reconcile the live index against disk when it is already anchored at
/// `canonical`, returning the same stats dict a full rebuild would.
///
/// Returns `None` — and rebuilds nothing — when the slot is empty or
/// holds a different root, which is the caller's signal to take the full
/// build path. The returned flag says whether the index actually
/// changed, so the caller can skip writing a snapshot that would be
/// byte-identical to the one already on disk.
///
/// The workspace mutex is held for the duration. A refresh mutates the
/// index in place rather than swapping a finished copy in, and readers
/// must not observe it half-reconciled. On an unchanged tree that window
/// is a stat walk; the pathological case, a refresh where every file
/// changed, is bounded by the full-rebuild cost it replaces.
fn refresh_in_place(
    index: &SharedIndex,
    canonical: &Path,
    resolver: Option<&HarnReferenceResolver>,
    started: Instant,
) -> Option<(VmValue, bool)> {
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = guard.as_mut()?;
    if state.root != *canonical {
        return None;
    }
    let outcome = state.refresh_from_root(resolver);
    let changed = !outcome.is_noop();
    let indexed_files = state.files.len() as i64;
    drop(guard);
    let elapsed_ms = started.elapsed().as_millis() as i64;
    tracing::info!(
        target: "harn_hostlib::code_index",
        root = %canonical.display(),
        files_scanned = outcome.files_scanned,
        files_unchanged = outcome.files_unchanged,
        files_reindexed = outcome.files_reindexed,
        files_added = outcome.files_added,
        files_removed = outcome.files_removed,
        elapsed_ms,
        "code-index incremental refresh complete",
    );
    Some((
        build_dict([
            ("mode", str_value("refreshed")),
            ("files_indexed", VmValue::Int(indexed_files)),
            ("files_skipped", VmValue::Int(outcome.files_skipped as i64)),
            (
                "files_reindexed",
                VmValue::Int(outcome.files_reindexed as i64),
            ),
            (
                "files_unchanged",
                VmValue::Int(outcome.files_unchanged as i64),
            ),
            ("files_added", VmValue::Int(outcome.files_added as i64)),
            ("files_removed", VmValue::Int(outcome.files_removed as i64)),
            ("elapsed_ms", VmValue::Int(elapsed_ms)),
        ]),
        changed,
    ))
}

/// Publish `canonical`'s on-disk snapshot, reconciled against disk, and
/// return the rebuild stats for it.
///
/// Returns `None` when there is no usable snapshot: none on disk, one for
/// another root or format version, or one that fails to decode. The caller
/// then takes the full build path. The caller must own the rebuild flight.
fn restore_for_rebuild(
    index: &SharedIndex,
    warm: &Arc<WarmCoordinator>,
    resolver: Option<&HarnReferenceResolver>,
    canonical: &Path,
    started: Instant,
) -> Option<VmValue> {
    let (needs_persist, outcome, files_indexed) =
        match super::restore_shared(index, resolver, canonical) {
            Ok(super::Restored::Published {
                needs_persist,
                outcome,
                files_indexed,
            }) => (needs_persist, outcome, files_indexed),
            Ok(super::Restored::Absent) => return None,
            Err(error) => {
                tracing::debug!(
                    target: "harn_hostlib::code_index",
                    %error,
                    root = %canonical.display(),
                    "code-index snapshot restore failed; falling back to rebuild",
                );
                return None;
            }
        };
    if needs_persist {
        // Counted before the caller ends the flight, so `wait_until_idle`
        // still covers the write.
        warm.persist_in_background(index.clone());
    }
    let elapsed_ms = started.elapsed().as_millis() as i64;
    tracing::info!(
        target: "harn_hostlib::code_index",
        root = %canonical.display(),
        files_indexed,
        files_reindexed = outcome.files_reindexed,
        elapsed_ms,
        "code-index rebuild restored snapshot",
    );
    Some(build_dict([
        ("mode", str_value("restored")),
        ("files_indexed", VmValue::Int(files_indexed as i64)),
        ("files_skipped", VmValue::Int(outcome.files_skipped as i64)),
        (
            "files_reindexed",
            VmValue::Int(outcome.files_reindexed as i64),
        ),
        (
            "files_unchanged",
            VmValue::Int(outcome.files_unchanged as i64),
        ),
        ("files_added", VmValue::Int(outcome.files_added as i64)),
        ("files_removed", VmValue::Int(outcome.files_removed as i64)),
        ("elapsed_ms", VmValue::Int(elapsed_ms)),
    ]))
}

/// Rebuild that joins any in-flight session warm for the same root.
pub(super) fn run_rebuild_single_flight(
    index: &SharedIndex,
    warm: &Arc<WarmCoordinator>,
    resolver: Option<&HarnReferenceResolver>,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    use super::builtins::BUILTIN_REBUILD;

    let raw = dict_arg(BUILTIN_REBUILD, args)?;
    let dict = raw.as_ref();
    let force = optional_bool(BUILTIN_REBUILD, dict, "force", false)?;
    let root = optional_string(BUILTIN_REBUILD, dict, "root")?
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !root.exists() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_REBUILD,
            param: "root",
            message: format!("path `{}` does not exist", root.display()),
        });
    }
    if !root.is_dir() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_REBUILD,
            param: "root",
            message: format!("path `{}` is not a directory", root.display()),
        });
    }

    let canonical = canonicalize(&root);
    let started = Instant::now();

    // Own a rebuild flight when possible. Only join (return live stats without
    // rebuilding) when another warm/rebuild already owns this root — never skip
    // a refresh just because the slot is already populated. Callers use rebuild
    // after edits via `host_codeindex_ensure_initialised` / `ensure_fresh`.
    for _ in 0..3 {
        let Some(flight) = warm.join_or_begin(&canonical) else {
            // Another builder owns this root; wait for it instead of double-walking.
            warm.wait_if_building(&canonical);
            if let Some(stats) = live_stats_for_root(index, &canonical, started.elapsed()) {
                return Ok(stats);
            }
            continue;
        };

        // An index already anchored at this root is reconciled in place.
        // Only a cold slot, a different root, or an explicit `force` pays
        // the full walk-read-parse pass. This is what makes `ensure_fresh`
        // the freshness gate its name claims: on an unchanged tree the
        // whole call is one `stat` per file.
        if !force {
            if let Some((stats, changed)) = refresh_in_place(index, &canonical, resolver, started) {
                let pending = changed.then(|| warm.begin_persist());
                drop(flight);
                // Only a refresh that changed something is worth writing
                // back. The snapshot is a whole-index serialisation, so
                // persisting after a no-op would put a multi-hundred-
                // megabyte write on the read path we just made free.
                if pending.is_some() {
                    if let Err(error) = super::persist_shared(index) {
                        tracing::debug!(
                            target: "harn_hostlib::code_index",
                            %error,
                            root = %canonical.display(),
                            "code-index refresh persist failed",
                        );
                    }
                }
                return Ok(stats);
            }
            // A slot that is empty or anchored elsewhere starts from this
            // root's snapshot when one is valid, so a new process pays a
            // load and a stat walk instead of a full parse.
            if let Some(stats) = restore_for_rebuild(index, warm, resolver, &canonical, started) {
                drop(flight);
                return Ok(stats);
            }
        }

        tracing::info!(
            target: "harn_hostlib::code_index",
            root = %canonical.display(),
            force,
            "code-index full rebuild starting",
        );
        let (mut state, outcome) = IndexState::build_from_root(&canonical);
        state.relink_harn_references(resolver);
        {
            let mut guard = index.lock().expect("code_index mutex poisoned");
            *guard = Some(state);
        }
        if let Err(error) = super::persist_shared(index) {
            tracing::debug!(
                target: "harn_hostlib::code_index",
                %error,
                root = %canonical.display(),
                "code-index rebuild persist failed",
            );
        }
        drop(flight);
        let elapsed_ms = started.elapsed().as_millis() as i64;
        tracing::info!(
            target: "harn_hostlib::code_index",
            root = %canonical.display(),
            files_indexed = outcome.files_indexed,
            files_skipped = outcome.files_skipped,
            elapsed_ms,
            "code-index rebuild complete",
        );
        return Ok(build_dict([
            ("mode", str_value("built")),
            ("files_indexed", VmValue::Int(outcome.files_indexed as i64)),
            ("files_skipped", VmValue::Int(outcome.files_skipped as i64)),
            // A cold build reads every file it indexes and adds every one
            // of them, so the reconciliation counters stay meaningful
            // rather than being reported as zero work.
            (
                "files_reindexed",
                VmValue::Int(outcome.files_indexed as i64),
            ),
            ("files_unchanged", VmValue::Int(0)),
            ("files_added", VmValue::Int(outcome.files_indexed as i64)),
            ("files_removed", VmValue::Int(0)),
            ("elapsed_ms", VmValue::Int(elapsed_ms)),
        ]));
    }

    // Exhausted join attempts; return whatever is live (possibly empty).
    Ok(
        live_stats_for_root(index, &canonical, started.elapsed()).unwrap_or_else(|| {
            build_dict([
                ("mode", str_value("refreshed")),
                ("files_indexed", VmValue::Int(0)),
                ("files_skipped", VmValue::Int(0)),
                ("files_reindexed", VmValue::Int(0)),
                ("files_unchanged", VmValue::Int(0)),
                ("files_added", VmValue::Int(0)),
                ("files_removed", VmValue::Int(0)),
                (
                    "elapsed_ms",
                    VmValue::Int(started.elapsed().as_millis() as i64),
                ),
            ])
        }),
    )
}

#[cfg(test)]
mod tests {
    use harn_vm::value::VmDictExt;
    #[cfg(target_os = "linux")]
    #[test]
    fn the_warm_thread_runs_below_normal_priority() {
        let nice = harn_parser::runtime_stack::spawn(|| {
            super::lower_to_background_priority();
            // SAFETY: reads the calling thread's own nice value.
            unsafe {
                let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
                libc::getpriority(libc::PRIO_PROCESS, tid)
            }
        })
        .join()
        .expect("priority thread");
        assert_eq!(nice, super::BACKGROUND_NICE);
    }

    use super::*;
    use std::fs;
    use std::time::Duration;

    use super::super::snapshot::CodeIndexSnapshot;

    fn fixture_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/alpha.rs"),
            "pub fn alpha() -> i32 { 1 }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/beta.py"),
            "def beta():\n    return 2\n",
        )
        .unwrap();
        dir
    }

    fn root_arg(root: &Path) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        map.put_str("root", root.to_string_lossy().as_ref());
        VmValue::dict(map)
    }

    #[test]
    fn a_scheduled_snapshot_write_keeps_the_coordinator_busy() {
        let warm = Arc::new(WarmCoordinator::default());
        let idle = |warm: &WarmCoordinator| warm.state.lock().unwrap().is_idle();
        assert!(idle(&warm));
        let first = warm.begin_persist();
        let second = warm.begin_persist();
        // No build is in flight, yet a waiter must still wait for the writes.
        assert!(warm.state.lock().unwrap().in_flight_root.is_none());
        assert!(!idle(&warm));
        drop(first);
        assert!(!idle(&warm), "one write is still outstanding");
        drop(second);
        assert!(idle(&warm));
        warm.wait_until_idle();
    }

    #[test]
    fn warm_session_restores_existing_snapshot() {
        let dir = fixture_tree();
        let seed = CodeIndexCapability::new();
        let (state, _) = IndexState::build_from_root(dir.path());
        {
            let shared = seed.shared();
            let mut guard = shared.lock().unwrap();
            *guard = Some(state);
        }
        seed.persist_to_disk().unwrap();

        let cold = CodeIndexCapability::new();
        assert_eq!(cold.warm_session(dir.path()), SessionWarmOutcome::Building);
        cold.wait_until_idle();
        let shared = cold.shared();
        let guard = shared.lock().unwrap();
        assert_eq!(guard.as_ref().map(|s| s.files.len()), Some(2));
    }

    /// harn#9272: a stale snapshot used to be reconciled on the caller's
    /// thread, so the session's first prompt waited minutes for the refresh.
    /// The warm must hand back `Building` before any snapshot work, then
    /// install and persist the reconciled index from the background flight.
    #[test]
    fn warm_session_reconciles_stale_snapshot_off_the_caller_thread() {
        let dir = fixture_tree();
        let (state, _) = IndexState::build_from_root(dir.path());
        let mut snap = state.snapshot();
        snap.meta.git_head = Some("ffffffffffffffff".to_string());
        snap.save(dir.path()).unwrap();
        fs::write(
            dir.path().join("src/gamma.rs"),
            "pub fn gamma() -> i32 { 3 }\n",
        )
        .unwrap();

        let cap = CodeIndexCapability::new();
        assert_eq!(cap.warm_session(dir.path()), SessionWarmOutcome::Building);
        cap.wait_until_idle();

        let shared = cap.shared();
        let guard = shared.lock().unwrap();
        assert_eq!(guard.as_ref().map(|s| s.files.len()), Some(3));
        drop(guard);
        let persisted = CodeIndexSnapshot::load(dir.path())
            .unwrap()
            .expect("reconciled snapshot");
        assert_eq!(
            persisted.meta.file_count, 3,
            "the background restore must persist the reconciled index"
        );
    }

    #[test]
    fn warm_session_builds_in_background_without_blocking() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        assert_eq!(cap.warm_session(dir.path()), SessionWarmOutcome::Building);
        // `Building` is the non-blocking handoff. The index slot is installed
        // before snapshot persistence, so observing the slot alone does not
        // prove that the whole flight finished. Join the owning coordinator:
        // its guard drops only after persistence has been attempted.
        cap.wait_until_idle();
        let shared = cap.shared();
        let guard = shared.lock().unwrap();
        assert_eq!(guard.as_ref().map(|state| state.files.len()), Some(2));
        drop(guard);
        assert!(
            CodeIndexSnapshot::path_for(dir.path()).exists(),
            "warm should persist a snapshot for the next session"
        );
    }

    #[test]
    fn sync_rebuild_persists_snapshot() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        let args = [root_arg(dir.path())];
        run_rebuild_single_flight(&cap.shared(), &cap.warm, None, &args).expect("rebuild");
        assert!(
            CodeIndexSnapshot::path_for(dir.path()).exists(),
            "sync rebuild is the path that always completes, so it must persist"
        );
    }

    #[test]
    fn live_stats_for_root_reports_join_wait_elapsed() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        let args = [root_arg(dir.path())];
        run_rebuild_single_flight(&cap.shared(), &cap.warm, None, &args).expect("seed");
        let canonical = canonicalize(dir.path());
        let stats = live_stats_for_root(&cap.shared(), &canonical, Duration::from_millis(41_100))
            .expect("live stats");
        let dict = match stats {
            VmValue::Dict(d) => d,
            other => panic!("expected dict, got {other:?}"),
        };
        match dict.get(&harn_vm::value::intern_key("elapsed_ms")) {
            Some(VmValue::Int(n)) => assert_eq!(*n, 41_100),
            other => panic!("expected elapsed_ms int, got {other:?}"),
        }
    }

    #[test]
    fn emit_wait_progress_pushes_tool_progress_for_current_session() {
        let session = "code-index-wait-progress";
        let _guard = harn_vm::agent_sessions::enter_current_session(session);
        emit_wait_progress(Path::new("/tmp/workspace"), Duration::from_millis(1500));
        let entries = harn_vm::orchestration::agent_inbox::drain(session);
        assert!(
            entries.iter().any(|entry| {
                entry.kind == "tool_progress"
                    && entry.source == "hostlib.code_index.rebuild"
                    && entry.content.contains("code_index_rebuild_wait")
                    && entry.content.contains("1500")
            }),
            "wait progress must be visible without a tracing subscriber; entries={entries:?}"
        );
    }

    #[test]
    fn sync_rebuild_joins_in_flight_warm() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        assert_eq!(cap.warm_session(dir.path()), SessionWarmOutcome::Building);

        let args = [root_arg(dir.path())];
        let started = Instant::now();
        let result =
            run_rebuild_single_flight(&cap.shared(), &cap.warm, None, &args).expect("rebuild");
        let elapsed = started.elapsed();
        let dict = match result {
            VmValue::Dict(d) => d,
            other => panic!("expected dict, got {other:?}"),
        };
        let files = match dict
            .get(&harn_vm::value::intern_key("files_indexed"))
            .unwrap()
        {
            VmValue::Int(n) => *n,
            other => panic!("expected int, got {other:?}"),
        };
        assert_eq!(files, 2);
        assert!(elapsed < Duration::from_secs(30));
    }

    #[test]
    fn sync_rebuild_refreshes_already_live_index_after_disk_change() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        let args = [root_arg(dir.path())];
        let first = run_rebuild_single_flight(&cap.shared(), &cap.warm, None, &args).expect("seed");
        let first_files = match first {
            VmValue::Dict(d) => match d.get(&harn_vm::value::intern_key("files_indexed")) {
                Some(VmValue::Int(n)) => *n,
                other => panic!("expected files_indexed int, got {other:?}"),
            },
            other => panic!("expected dict, got {other:?}"),
        };
        assert_eq!(first_files, 2);

        fs::write(
            dir.path().join("src/gamma.rs"),
            "pub fn gamma() -> i32 { 3 }\n",
        )
        .unwrap();

        let second =
            run_rebuild_single_flight(&cap.shared(), &cap.warm, None, &args).expect("refresh");
        let second_files = match second {
            VmValue::Dict(d) => match d.get(&harn_vm::value::intern_key("files_indexed")) {
                Some(VmValue::Int(n)) => *n,
                other => panic!("expected files_indexed int, got {other:?}"),
            },
            other => panic!("expected dict, got {other:?}"),
        };
        assert_eq!(
            second_files, 3,
            "rebuild must re-walk disk even when the slot is already live"
        );
        let shared = cap.shared();
        let guard = shared.lock().unwrap();
        assert_eq!(guard.as_ref().map(|s| s.files.len()), Some(3));
    }

    /// Rebuild through the builtin path, checked against its response schema.
    fn schema_checked_rebuild(cap: &CodeIndexCapability, root: &Path) -> VmValue {
        let out = run_rebuild_single_flight(&cap.shared(), &cap.warm, None, &[root_arg(root)])
            .expect("rebuild");
        crate::schemas::validate_response(
            super::super::builtins::BUILTIN_REBUILD,
            "code_index",
            "rebuild",
            out,
        )
        .expect("rebuild response matches its schema")
    }

    fn get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
        let VmValue::Dict(dict) = value else {
            panic!("expected dict, got {value:?}");
        };
        dict.get(&harn_vm::value::intern_key(key))
            .unwrap_or_else(|| panic!("missing `{key}` in {value:?}"))
    }

    fn int(value: &VmValue, key: &str) -> i64 {
        match get(value, key) {
            VmValue::Int(n) => *n,
            other => panic!("expected int at `{key}`, got {other:?}"),
        }
    }

    fn string(value: &VmValue, key: &str) -> String {
        match get(value, key) {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string at `{key}`, got {other:?}"),
        }
    }

    /// A new process over a tree a previous process indexed starts from the
    /// snapshot that process left, rather than walking and parsing again.
    #[test]
    fn a_new_process_rebuild_restores_the_snapshot_instead_of_walking() {
        let dir = fixture_tree();
        let writer = CodeIndexCapability::new();
        let first = schema_checked_rebuild(&writer, dir.path());
        assert_eq!(string(&first, "mode"), "built");
        writer.wait_until_idle();
        assert!(CodeIndexSnapshot::path_for(dir.path()).exists());

        let reader = CodeIndexCapability::new();
        let restored = schema_checked_rebuild(&reader, dir.path());
        assert_eq!(string(&restored, "mode"), "restored");
        assert_eq!(int(&restored, "files_indexed"), 2);
        assert_eq!(
            int(&restored, "files_reindexed"),
            0,
            "a restore of an unchanged tree must not read a single file"
        );
        assert_eq!(int(&restored, "files_unchanged"), 2);

        // A file written while no process was running is still picked up.
        fs::write(
            dir.path().join("src/gamma.rs"),
            "pub fn gamma() -> i32 { 3 }\n",
        )
        .unwrap();
        let next = CodeIndexCapability::new();
        let reconciled = schema_checked_rebuild(&next, dir.path());
        assert_eq!(string(&reconciled, "mode"), "restored");
        assert_eq!(int(&reconciled, "files_added"), 1);
        assert_eq!(int(&reconciled, "files_indexed"), 3);
        next.wait_until_idle();
    }

    /// Only a snapshot that describes this root, in this format, and decodes
    /// cleanly is restored. Anything else falls back to the full build.
    #[test]
    fn a_snapshot_that_does_not_describe_the_root_is_rebuilt() {
        let other = fixture_tree();
        let other_writer = CodeIndexCapability::new();
        schema_checked_rebuild(&other_writer, other.path());
        other_writer.wait_until_idle();
        let other_bytes = fs::read(CodeIndexSnapshot::path_for(other.path())).unwrap();

        type Mangle = fn(Vec<u8>, &[u8]) -> Vec<u8>;
        let cases: [(&str, Mangle); 3] = [
            ("another root's snapshot", |_, other| other.to_vec()),
            ("another format version", |mut own, _| {
                own[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
                own
            }),
            ("a truncated snapshot", |mut own, _| {
                own.truncate(own.len() / 2);
                own
            }),
        ];
        for (label, mangle) in cases {
            let dir = fixture_tree();
            let writer = CodeIndexCapability::new();
            schema_checked_rebuild(&writer, dir.path());
            writer.wait_until_idle();
            let path = CodeIndexSnapshot::path_for(dir.path());
            let own = fs::read(&path).unwrap();
            fs::write(&path, mangle(own, &other_bytes)).unwrap();

            let reader = CodeIndexCapability::new();
            let out = schema_checked_rebuild(&reader, dir.path());
            assert_eq!(string(&out, "mode"), "built", "{label}");
            assert_eq!(int(&out, "files_reindexed"), 2, "{label}");
            let guard = reader.index.lock().unwrap();
            assert_eq!(
                guard.as_ref().map(|state| state.root.clone()),
                Some(canonicalize(dir.path())),
                "{label}"
            );
        }
    }

    fn schema_checked_status(cap: &CodeIndexCapability) -> VmValue {
        let out = super::super::builtins::run_status(&cap.shared(), &[]).expect("status");
        crate::schemas::validate_response(
            super::super::builtins::BUILTIN_STATUS,
            "code_index",
            "status",
            out,
        )
        .expect("status response matches its schema")
    }

    /// A caller can tell an index for its own root from one for another root
    /// or from no index at all.
    #[test]
    fn status_reports_the_root_the_live_index_belongs_to() {
        let cap = CodeIndexCapability::new();
        assert!(matches!(
            get(&schema_checked_status(&cap), "root"),
            VmValue::Nil
        ));

        let dir = fixture_tree();
        schema_checked_rebuild(&cap, dir.path());
        assert_eq!(
            string(&schema_checked_status(&cap), "root"),
            crate::tools::args::to_agent_path(canonicalize(dir.path()))
        );

        let other = fixture_tree();
        schema_checked_rebuild(&cap, other.path());
        assert_eq!(
            string(&schema_checked_status(&cap), "root"),
            crate::tools::args::to_agent_path(canonicalize(other.path()))
        );
        cap.wait_until_idle();
    }
}
