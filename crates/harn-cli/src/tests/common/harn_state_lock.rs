use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

static HARN_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Process-global env vars that point harn_vm at a specific state dir
/// or flip the MCP serve auth posture. Any test that leaves these set
/// leaks state into subsequent tests:
/// - State-dir vars leak the previous test's (now-deleted) `TempDir`
///   path into `install_default_for_base_dir(base_dir)` because
///   `state_root()` / `event_log_*` resolvers honor an absolute env-var
///   value over the supplied `base_dir`.
/// - `HARN_MCP_OAUTH_*` vars flip `McpOrchestratorService::new_local`
///   into OAuth-required mode, so a test that constructs a service
///   while a previous OAuth test's env is still live receives 401 on
///   every unauthenticated request.
const LEAKY_STATE_ENV_VARS: &[&str] = &[
    harn_vm::runtime_paths::HARN_STATE_DIR_ENV,
    harn_vm::runtime_paths::HARN_RUN_DIR_ENV,
    harn_vm::runtime_paths::HARN_WORKTREE_DIR_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_DIR_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV,
    harn_vm::event_log::HARN_EVENT_LOG_QUEUE_DEPTH_ENV,
    "HARN_MCP_OAUTH_AUTHORIZATION_SERVERS",
    "HARN_MCP_OAUTH_INTROSPECTION_URL",
    "HARN_MCP_OAUTH_RESOURCE",
    "HARN_MCP_OAUTH_AUDIENCE",
    "HARN_MCP_OAUTH_SCOPES",
];

/// Clear the process-global env vars that leak state between tests. Run
/// on every lock acquisition so each test starts from a clean env
/// instead of inheriting a previous test's absolute state path.
fn clear_leaky_state_env() {
    for name in LEAKY_STATE_ENV_VARS {
        std::env::remove_var(name);
    }
}

/// The `tokio::sync::Mutex` backing both the sync and async acquire
/// paths. A `tokio` mutex (rather than `std::sync::Mutex`) lets async
/// tests hold the guard across `.await` points without tripping
/// `clippy::await_holding_lock`, while still offering a blocking acquire
/// for the handful of plain `#[test]` callers.
fn state_mutex() -> &'static Mutex<()> {
    HARN_STATE_LOCK.get_or_init(|| Mutex::new(()))
}

/// Serialize plain `#[test]` callers that mutate harn_vm process-global
/// state. Async tests must use [`lock_harn_state_async`] instead —
/// `blocking_lock` panics when called from within a tokio runtime.
///
/// Covers:
/// - `HARN_STATE_DIR` and sibling env vars read by
///   `harn_vm::runtime_paths::state_root()` / `event_log_*`. The lock
///   helper unsets them on entry so each test starts from a clean env
///   instead of inheriting a previous test's absolute state path. No
///   production code writes them any more — `OrchestratorRole::build_vm()`
///   used to, and every test in the binary inherited its state dir — so
///   this now guards only tests that set them deliberately.
/// - The thread-local `ACTIVE_EVENT_LOG`, which is reused across
///   cargo test-thread handoffs.
/// - The process-global `harn_vm` trigger registry mutated by
///   `install_manifest_triggers` / `clear_trigger_registry`.
///
/// Tests grabbing this lock should not assume the global state is clean
/// on entry — always call `reset_active_event_log()` +
/// `harn_vm::clear_trigger_registry()` as applicable.
pub fn lock_harn_state() -> MutexGuard<'static, ()> {
    let guard = state_mutex().blocking_lock();
    clear_leaky_state_env();
    guard
}

/// Async variant for `#[tokio::test]` callers that hold the state guard
/// across `.await`. Same env-clearing semantics as [`lock_harn_state`].
pub async fn lock_harn_state_async() -> MutexGuard<'static, ()> {
    let guard = state_mutex().lock().await;
    clear_leaky_state_env();
    guard
}
