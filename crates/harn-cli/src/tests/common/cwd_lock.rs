use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Serialize tests that mutate the process-wide current directory.
pub(crate) fn lock_cwd() -> MutexGuard<'static, ()> {
    CWD_LOCK.get_or_init(|| Mutex::new(())).blocking_lock()
}

/// Async variant for tests that hold the cwd lock across `.await`.
pub(crate) async fn lock_cwd_async() -> MutexGuard<'static, ()> {
    CWD_LOCK.get_or_init(|| Mutex::new(())).lock().await
}
