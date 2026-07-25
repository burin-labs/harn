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

/// Whoever currently holds the lock, recorded so that a second acquire
/// from the same holder fails loudly instead of hanging.
///
/// A tokio task id is stable across worker threads, so it stays correct
/// under `flavor = "multi_thread"` where a thread id would not. Plain
/// `#[test]` callers run outside any runtime and get a thread id, which
/// is exact for them because each such test owns its thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holder {
    Task(tokio::task::Id),
    Thread(std::thread::ThreadId),
}

static HOLDER: std::sync::Mutex<Option<Holder>> = std::sync::Mutex::new(None);

fn current_holder() -> Holder {
    match tokio::task::try_id() {
        Some(id) => Holder::Task(id),
        None => Holder::Thread(std::thread::current().id()),
    }
}

/// Panic if the caller already holds the lock.
///
/// Re-acquiring a non-reentrant mutex deadlocks, and a deadlocked test
/// binary reports nothing at all — it just stops, and the run has to be
/// killed by hand to learn anything. Reading `HOLDER` before we block is
/// sound: the only value that can name us is one we wrote ourselves, and
/// nobody else can write our name.
fn reject_reentrant_acquire() {
    let holder = current_holder();
    let already_held = *HOLDER.lock().expect("harn-state holder") == Some(holder);
    assert!(
        !already_held,
        "this test already holds the harn-state lock; acquiring it again would deadlock. \
         There is exactly one lock over the process environment — take it once, at the top \
         of the test, and pass the guard down if an inner helper needs it."
    );
}

/// Guard for the process-global harn-state lock. Releasing it clears the
/// recorded holder, so the next acquire from the same task is legal
/// again.
pub struct HarnStateGuard {
    _inner: MutexGuard<'static, ()>,
}

impl Drop for HarnStateGuard {
    fn drop(&mut self) {
        *HOLDER.lock().expect("harn-state holder") = None;
    }
}

fn finish_acquire(inner: MutexGuard<'static, ()>) -> HarnStateGuard {
    *HOLDER.lock().expect("harn-state holder") = Some(current_holder());
    clear_leaky_state_env();
    HarnStateGuard { _inner: inner }
}

/// Serialize plain `#[test]` callers that mutate harn_vm process-global
/// state. Async tests must use [`lock_harn_state_async`] instead —
/// `blocking_lock` panics when called from within a tokio runtime.
///
/// This is the *only* lock over the process environment in this crate.
/// It used to have a sibling, `env_lock`, and two mutexes over one
/// environment exclude nothing: a test holding one ran concurrently with
/// a test holding the other and clobbered its `HARN_STATE_DIR` /
/// `HARN_EVENT_LOG_*`. Do not reintroduce a second lock for the same
/// state; add the variable to [`LEAKY_STATE_ENV_VARS`] instead.
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
pub fn lock_harn_state() -> HarnStateGuard {
    reject_reentrant_acquire();
    finish_acquire(state_mutex().blocking_lock())
}

/// Async variant for `#[tokio::test]` callers that hold the state guard
/// across `.await`. Same env-clearing semantics as [`lock_harn_state`].
pub async fn lock_harn_state_async() -> HarnStateGuard {
    reject_reentrant_acquire();
    finish_acquire(state_mutex().lock().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this guards against used to present as a test binary
    /// that produced no output and never exited. Re-acquiring must fail
    /// in the same breath instead.
    #[tokio::test]
    #[should_panic(expected = "already holds the harn-state lock")]
    async fn a_second_acquire_from_the_same_test_panics_instead_of_hanging() {
        let _first = lock_harn_state_async().await;
        let _second = lock_harn_state_async().await;
    }

    /// Releasing has to clear the recorded holder, or a task that legally
    /// takes the lock twice in sequence would trip the detector.
    #[tokio::test]
    async fn releasing_the_lock_lets_the_same_test_take_it_again() {
        drop(lock_harn_state_async().await);
        let _second = lock_harn_state_async().await;
    }
}
