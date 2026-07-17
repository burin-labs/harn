//! Session-keyed changed-path tracking.
//!
//! A process-global map of the paths each session has mutated, recorded at the
//! hostlib write chokepoint and read back into a sub-agent's `files_written`
//! receipt. It is split out of the parent module so this session-keyed global
//! and the drains that keep a reused session id from inheriting a prior run's
//! writes live in one small, auditable unit — the reset backstop that teardown
//! cannot be is right next to the store it guards.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

/// Per-session set of filesystem paths a session has mutated (written or
/// deleted) via the deterministic hostlib write surface. Keyed by session id
/// and process-global (not thread-local like the `SESSIONS` store) so a
/// background fan-out child's writes are attributed correctly regardless of
/// which runtime thread serviced them. Fed by the single hostlib write
/// chokepoint (`harn-hostlib`'s `fs_snapshot::auto_capture_for_write`), which
/// is reached only AFTER a write is policy-approved and about to touch disk —
/// so this reflects ACTUAL committed mutations, not denied/aborted attempts.
/// The authoritative source for a sub-agent's `files_written` receipt field.
static SESSION_CHANGED_PATHS: OnceLock<Mutex<BTreeMap<String, BTreeSet<String>>>> = OnceLock::new();

fn session_changed_paths_store() -> &'static Mutex<BTreeMap<String, BTreeSet<String>>> {
    SESSION_CHANGED_PATHS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record that `path` was mutated (written/deleted) by `session_id`. Called from
/// the hostlib write chokepoint; a no-op when either argument is empty.
pub fn record_session_changed_path(session_id: &str, path: &str) {
    if session_id.is_empty() || path.is_empty() {
        return;
    }
    if let Ok(mut store) = session_changed_paths_store().lock() {
        store
            .entry(session_id.to_string())
            .or_default()
            .insert(path.to_string());
    }
}

/// The sorted set of paths `session_id` has mutated so far (empty when none).
/// Non-draining: safe to call for introspection without disturbing the record.
pub fn session_changed_paths(session_id: &str) -> Vec<String> {
    session_changed_paths_store()
        .lock()
        .ok()
        .and_then(|store| {
            store
                .get(session_id)
                .map(|set| set.iter().cloned().collect())
        })
        .unwrap_or_default()
}

/// Read AND remove a session's mutated-path set. Used at sub-agent teardown so
/// the receipt captures the child's writes exactly once and the global map does
/// not grow unbounded across a long-lived daemon's many fan-outs.
pub fn take_session_changed_paths(session_id: &str) -> Vec<String> {
    session_changed_paths_store()
        .lock()
        .ok()
        .and_then(|mut store| store.remove(session_id))
        .map(|set| set.into_iter().collect())
        .unwrap_or_default()
}

/// Drop a session's recorded mutated paths (explicit teardown / test reset).
pub fn clear_session_changed_paths(session_id: &str) {
    if let Ok(mut store) = session_changed_paths_store().lock() {
        store.remove(session_id);
    }
}

/// Drop EVERY session's recorded mutated paths.
///
/// The per-session removals above run at teardown, which a session that errors
/// or is abandoned never reaches — and this map is process-global while the
/// session store beside it is thread-local, so its entries outlive the sessions
/// that made them. A later session reusing an id would then inherit the earlier
/// one's paths into its `files_written` receipt: a receipt reporting writes that
/// this run did not make. Reset is the backstop teardown cannot be.
pub fn clear_all_session_changed_paths() {
    if let Ok(mut store) = session_changed_paths_store().lock() {
        store.clear();
    }
}
