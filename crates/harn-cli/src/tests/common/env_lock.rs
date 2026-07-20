use std::sync::OnceLock;

use tokio::sync::Mutex;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Serialize tests that mutate process-wide environment variables.
///
/// A test that also touches the process-global run-event sink (e.g. it runs a
/// pipeline) must take `run_event_sink_lock` too. To stay deadlock-free, always
/// acquire this lock first, then the sink lock — the one fixed order both locks
/// document.
pub fn lock_env() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}
