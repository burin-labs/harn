use std::sync::{Mutex, MutexGuard, OnceLock};

static HARN_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Process-global env vars that point harn_vm at a specific state dir.
/// Any test that leaves these set leaks its (now-deleted) `TempDir`
/// path into subsequent tests' `install_default_for_base_dir(base_dir)`
/// calls because `state_root()` / `event_log_*` resolvers honor an
/// absolute env-var value over the supplied `base_dir`, silently
/// pointing every test at the same stale SQLite file.
const LEAKY_STATE_ENV_VARS: &[&str] = &[
    harn_vm::runtime_paths::HARN_STATE_DIR_ENV,
    harn_vm::runtime_paths::HARN_RUN_DIR_ENV,
    harn_vm::runtime_paths::HARN_WORKTREE_DIR_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_DIR_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_QUEUE_DEPTH_ENV,
];

/// Serialize tests that mutate harn_vm process-global state.
///
/// Covers:
/// - `HARN_STATE_DIR` and sibling env vars read by
///   `harn_vm::runtime_paths::state_root()` / `event_log_*` and written
///   by `OrchestratorRole::build_vm()`. The lock helper unsets them on
///   entry so each test starts from a clean env instead of inheriting
///   a previous test's absolute state path.
/// - The thread-local `ACTIVE_EVENT_LOG`, which is reused across
///   cargo test-thread handoffs.
/// - The process-global `harn_vm` trigger registry mutated by
///   `install_manifest_triggers` / `clear_trigger_registry`.
///
/// Tests grabbing this lock should not assume the global state is clean
/// on entry — always call `reset_active_event_log()` +
/// `harn_vm::clear_trigger_registry()` as applicable.
///
/// Poison recovery: a prior panic may poison the mutex. We recover the
/// guard because each test resets the state on entry, so the mutex's
/// `()` payload is irrelevant and propagating `PoisonError` would
/// cascade a single legitimate failure across every downstream test.
pub(crate) fn lock_harn_state() -> MutexGuard<'static, ()> {
    let guard = HARN_STATE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    for name in LEAKY_STATE_ENV_VARS {
        std::env::remove_var(name);
    }
    guard
}
