use std::sync::OnceLock;

use tokio::sync::Mutex;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Serialize tests that mutate process-wide environment variables.
pub(crate) fn lock_env() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}
