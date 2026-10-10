//! Session-keyed changed-path tracking.
//!
//! A process-global map of the paths each session has mutated, recorded at the
//! hostlib write chokepoint and read back into a sub-agent's `files_written`
//! receipt. It is split out of the parent module so this session-keyed global
//! and the owner-scoped drains that keep a reused session id from inheriting a
//! prior run's writes live in one small, auditable unit.

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

/// Drop a session's recorded mutated paths (explicit teardown / test reset),
/// including every per-tool-call record it still holds.
pub fn clear_session_changed_paths(session_id: &str) {
    if let Ok(mut store) = session_changed_paths_store().lock() {
        store.remove(session_id);
    }
    if let Ok(mut store) = tool_call_mutations_store().lock() {
        store.remove(session_id);
    }
}

/// Completed workspace mutations, by session and then by the tool call that
/// made them. A call the loop abandons at exit has no result of its own; this
/// record is what lets it still report that it changed the workspace.
type ToolCallMutations = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

static TOOL_CALL_MUTATIONS: OnceLock<Mutex<ToolCallMutations>> = OnceLock::new();

fn tool_call_mutations_store() -> &'static Mutex<ToolCallMutations> {
    TOOL_CALL_MUTATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record that the current tool call finished mutating `path`.
///
/// Call it only AFTER the mutation succeeded, never before: a write that
/// failed changed nothing, and an abandoned call must not report it as
/// applied. A no-op outside an active session or tool-call scope.
pub fn record_tool_call_mutation(path: &str) {
    let Some(session_id) = super::current_session_id().filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(tool_call_id) = super::current_tool_call_id().filter(|id| !id.is_empty()) else {
        return;
    };
    record_tool_call_mutation_for(&session_id, &tool_call_id, path);
}

/// [`record_tool_call_mutation`] for an explicit session and call.
pub(crate) fn record_tool_call_mutation_for(session_id: &str, tool_call_id: &str, path: &str) {
    if session_id.is_empty() || tool_call_id.is_empty() || path.is_empty() {
        return;
    }
    let (session_id, tool_call_id) = (session_id.to_string(), tool_call_id.to_string());
    if let Ok(mut store) = tool_call_mutations_store().lock() {
        store
            .entry(session_id)
            .or_default()
            .entry(tool_call_id)
            .or_default()
            .insert(path.to_string());
    }
}

/// Read and release the paths `tool_call_id` finished mutating in
/// `session_id`, sorted; empty when it recorded none.
pub fn take_tool_call_mutations(session_id: &str, tool_call_id: &str) -> Vec<String> {
    let Ok(mut store) = tool_call_mutations_store().lock() else {
        return Vec::new();
    };
    let Some(calls) = store.get_mut(session_id) else {
        return Vec::new();
    };
    let paths = calls.remove(tool_call_id).unwrap_or_default();
    if calls.is_empty() {
        store.remove(session_id);
    }
    paths.into_iter().collect()
}

/// Drop every session's recorded mutated paths at process teardown.
///
/// Ordinary execution and thread reset must use [`clear_session_changed_paths`]
/// for only the sessions it owns. This process-global wipe exists for lifecycle
/// boundaries that have exclusive ownership of the whole process.
pub fn clear_all_session_changed_paths() {
    if let Ok(mut store) = session_changed_paths_store().lock() {
        store.clear();
    }
    if let Ok(mut store) = tool_call_mutations_store().lock() {
        store.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn resetting_one_owner_preserves_another_owners_changed_paths() {
        let owner_a_session = format!("changed-path-owner-a-{}", uuid::Uuid::now_v7());
        let owner_b_session = format!("changed-path-owner-b-{}", uuid::Uuid::now_v7());
        let owners_ready = Arc::new(Barrier::new(2));
        let owner_b_reset = Arc::new(Barrier::new(2));

        let owner_a_ready = Arc::clone(&owners_ready);
        let owner_a_reset = Arc::clone(&owner_b_reset);
        let owner_a = crate::runtime_stack::spawn(move || {
            super::super::open_or_create_for_test(Some(owner_a_session.clone()));
            record_session_changed_path(&owner_a_session, "src/owner-a.rs");
            owner_a_ready.wait();
            owner_a_reset.wait();

            assert_eq!(
                take_session_changed_paths(&owner_a_session),
                vec!["src/owner-a.rs".to_string()],
                "another owner's reset must not erase this owner's receipt"
            );
            super::super::reset_session_store();
        });

        let owner_b_ready = Arc::clone(&owners_ready);
        let owner_b_reset = Arc::clone(&owner_b_reset);
        let owner_b = crate::runtime_stack::spawn(move || {
            super::super::open_or_create_for_test(Some(owner_b_session));
            owner_b_ready.wait();
            super::super::reset_session_store();
            owner_b_reset.wait();
        });

        owner_a.join().expect("owner A");
        owner_b.join().expect("owner B");
    }

    #[test]
    fn resetting_or_closing_an_owner_removes_its_abandoned_changed_paths() {
        let reset_session = format!("changed-path-reset-{}", uuid::Uuid::now_v7());
        super::super::open_or_create_for_test(Some(reset_session.clone()));
        record_session_changed_path(&reset_session, "src/reset.rs");
        super::super::reset_session_store();
        assert!(
            session_changed_paths(&reset_session).is_empty(),
            "reset must discard paths owned by the reset session"
        );

        let closed_session = format!("changed-path-close-{}", uuid::Uuid::now_v7());
        super::super::open_or_create_for_test(Some(closed_session.clone()));
        record_session_changed_path(&closed_session, "src/close.rs");
        super::super::close(&closed_session);
        assert!(
            session_changed_paths(&closed_session).is_empty(),
            "close must discard paths owned by the closed session"
        );
    }

    #[test]
    fn opening_a_reused_session_id_discards_stale_changed_paths() {
        let session = format!("changed-path-reuse-{}", uuid::Uuid::now_v7());
        record_session_changed_path(&session, "src/stale.rs");

        super::super::open_or_create_for_test(Some(session.clone()));

        assert!(
            session_changed_paths(&session).is_empty(),
            "a fresh session must not inherit an abandoned owner's receipt"
        );
        super::super::close(&session);
    }

    #[test]
    fn evicting_an_owner_removes_its_abandoned_changed_paths() {
        let evicted_session = format!("changed-path-evicted-{}", uuid::Uuid::now_v7());
        super::super::set_session_cap(1);
        super::super::open_or_create_for_test(Some(evicted_session.clone()));
        record_session_changed_path(&evicted_session, "src/evicted.rs");

        super::super::open_or_create_for_test(Some(format!(
            "changed-path-replacement-{}",
            uuid::Uuid::now_v7()
        )));

        assert!(
            session_changed_paths(&evicted_session).is_empty(),
            "LRU eviction must discard paths owned by the evicted session"
        );
        super::super::reset_session_store();
        super::super::set_session_cap(super::super::DEFAULT_SESSION_CAP);
    }
}
