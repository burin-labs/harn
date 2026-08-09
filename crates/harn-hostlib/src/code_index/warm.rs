//! Non-blocking session warm for the code index.
//!
//! Embedders call [`CodeIndexCapability::warm_session`] at session start so a
//! cold workspace can restore a snapshot or begin a background rebuild without
//! stalling the model's first turn. Sync [`hostlib_code_index_rebuild`] joins
//! the same single-flight gate so `ensure_initialised` does not start a second
//! full walk while the warm is still running.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use harn_vm::VmValue;

use super::builtins::SharedIndex;
use super::state::{canonicalize, IndexState};
use super::CodeIndexCapability;
use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_bool, optional_string};

/// Outcome of [`CodeIndexCapability::warm_session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionWarmOutcome {
    /// In-memory index was already populated.
    AlreadyLive,
    /// Snapshot restore succeeded; the index is live now.
    Restored,
    /// A background rebuild is in flight (started now or already running).
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
}

/// Single-flight coordinator shared by session warm and sync rebuild.
#[derive(Debug)]
pub(super) struct WarmCoordinator {
    state: Mutex<WarmState>,
    cv: Condvar,
}

impl Default for WarmCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(WarmState {
                in_flight_root: None,
                generation: 0,
            }),
            cv: Condvar::new(),
        }
    }
}

/// RAII guard that clears the in-flight warm marker on drop (including panic).
struct WarmFlight {
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
        while guard
            .in_flight_root
            .as_ref()
            .is_some_and(|inflight| inflight == &canonical)
        {
            guard = self.cv.wait(guard).expect("warm coordinator poisoned");
        }
    }

    /// Mark `root` as building. Returns `None` when another builder already
    /// owns this root (caller should wait). Returns a drop-guard when this
    /// caller owns the flight.
    fn try_begin(self: &Arc<Self>, root: &Path) -> Option<WarmFlight> {
        let canonical = canonicalize(root);
        let mut guard = self.state.lock().expect("warm coordinator poisoned");
        if guard
            .in_flight_root
            .as_ref()
            .is_some_and(|inflight| inflight == &canonical)
        {
            return None;
        }
        // A different root in flight is rare (one capability per workspace).
        // Wait for it to finish so two full walks never race the shared slot.
        while guard.in_flight_root.is_some() {
            guard = self.cv.wait(guard).expect("warm coordinator poisoned");
        }
        guard.in_flight_root = Some(canonical.clone());
        Some(WarmFlight {
            warm: Arc::clone(self),
            root: canonical,
        })
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

impl CodeIndexCapability {
    /// Warm the shared index for `workspace_root` without blocking on a full
    /// cold rebuild.
    ///
    /// Order:
    /// 1. If the in-memory slot is already populated, return
    ///    [`SessionWarmOutcome::AlreadyLive`].
    /// 2. Try [`Self::restore_from_disk`].
    /// 3. Otherwise start (or join) a single-flight background
    ///    [`IndexState::build_from_root`], install it into the shared slot, and
    ///    [`Self::persist_to_disk`].
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

        match self.restore_from_disk(&root) {
            Ok(true) => return SessionWarmOutcome::Restored,
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    target: "harn_hostlib::code_index",
                    %error,
                    root = %root.display(),
                    "code-index snapshot restore failed; falling back to background rebuild",
                );
            }
        }

        let Some(flight) = self.warm.try_begin(&root) else {
            return SessionWarmOutcome::Building;
        };

        let index = self.index.clone();
        let capability = self.clone();
        let thread_root = root.clone();
        match thread::Builder::new()
            .name("harn-code-index-warm".to_string())
            .spawn(move || {
                let _flight = flight;
                let started = Instant::now();
                let (state, outcome) = IndexState::build_from_root(&thread_root);
                {
                    let mut guard = index.lock().expect("code_index mutex poisoned");
                    // Prefer an already-installed index (e.g. a finished sync
                    // rebuild that raced us after we began) over clobbering.
                    if guard.is_none() {
                        *guard = Some(state);
                    }
                }
                if let Err(error) = capability.persist_to_disk() {
                    tracing::debug!(
                        target: "harn_hostlib::code_index",
                        %error,
                        root = %thread_root.display(),
                        "code-index warm persist failed",
                    );
                }
                tracing::debug!(
                    target: "harn_hostlib::code_index",
                    root = %thread_root.display(),
                    files_indexed = outcome.files_indexed,
                    files_skipped = outcome.files_skipped,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "code-index background warm complete",
                );
            }) {
            Ok(_) => SessionWarmOutcome::Building,
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

fn live_stats_for_root(index: &SharedIndex, canonical: &Path) -> Option<VmValue> {
    let guard = index.lock().expect("code_index mutex poisoned");
    let state = guard.as_ref()?;
    if state.root != *canonical {
        return None;
    }
    Some(build_dict([
        ("files_indexed", VmValue::Int(state.files.len() as i64)),
        ("files_skipped", VmValue::Int(0)),
        ("elapsed_ms", VmValue::Int(0)),
    ]))
}

/// Rebuild that joins any in-flight session warm for the same root.
pub(super) fn run_rebuild_single_flight(
    index: &SharedIndex,
    warm: &Arc<WarmCoordinator>,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    use super::builtins::BUILTIN_REBUILD;

    let raw = dict_arg(BUILTIN_REBUILD, args)?;
    let dict = raw.as_ref();
    let _force = optional_bool(BUILTIN_REBUILD, dict, "force", false)?;
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

    // Join an in-flight warm/rebuild for this root so callers never pay a
    // second full walk.
    for _ in 0..3 {
        warm.wait_if_building(&canonical);
        if let Some(stats) = live_stats_for_root(index, &canonical) {
            return Ok(stats);
        }
        let Some(flight) = warm.try_begin(&canonical) else {
            // Lost the race to another builder; loop and join it.
            continue;
        };

        let started = Instant::now();
        let (state, outcome) = IndexState::build_from_root(&canonical);
        let elapsed_ms = started.elapsed().as_millis() as i64;
        {
            let mut guard = index.lock().expect("code_index mutex poisoned");
            *guard = Some(state);
        }
        drop(flight);
        return Ok(build_dict([
            ("files_indexed", VmValue::Int(outcome.files_indexed as i64)),
            ("files_skipped", VmValue::Int(outcome.files_skipped as i64)),
            ("elapsed_ms", VmValue::Int(elapsed_ms)),
        ]));
    }

    // Exhausted join attempts; return whatever is live (possibly empty).
    Ok(live_stats_for_root(index, &canonical).unwrap_or_else(|| {
        build_dict([
            ("files_indexed", VmValue::Int(0)),
            ("files_skipped", VmValue::Int(0)),
            ("elapsed_ms", VmValue::Int(0)),
        ])
    }))
}

#[cfg(test)]
mod tests {
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
        map.insert(
            harn_vm::value::intern_key("root"),
            VmValue::String(arcstr::ArcStr::from(root.to_string_lossy().as_ref())),
        );
        VmValue::dict(map)
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
        assert_eq!(cold.warm_session(dir.path()), SessionWarmOutcome::Restored);
        let shared = cold.shared();
        let guard = shared.lock().unwrap();
        assert_eq!(guard.as_ref().map(|s| s.files.len()), Some(2));
    }

    #[test]
    fn warm_session_builds_in_background_without_blocking() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        assert_eq!(cap.warm_session(dir.path()), SessionWarmOutcome::Building);
        // Immediately after kickoff the slot may still be empty — that is the
        // non-blocking contract. Wait for the background thread.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            {
                let shared = cap.shared();
                let guard = shared.lock().unwrap();
                if guard.as_ref().is_some_and(|s| s.files.len() == 2) {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "background warm did not populate the index in time"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            CodeIndexSnapshot::path_for(dir.path()).exists(),
            "warm should persist a snapshot for the next session"
        );
    }

    #[test]
    fn sync_rebuild_joins_in_flight_warm() {
        let dir = fixture_tree();
        let cap = CodeIndexCapability::new();
        assert_eq!(cap.warm_session(dir.path()), SessionWarmOutcome::Building);

        let args = [root_arg(dir.path())];
        let started = Instant::now();
        let result = run_rebuild_single_flight(&cap.shared(), &cap.warm, &args).expect("rebuild");
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
}
