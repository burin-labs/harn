use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

static RUN_EVENT_SINK_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Serialize tests that touch the process-global `harn_vm::run_events` sink.
///
/// The sink slot is process-wide: a `--json` run installs a sink there, while a
/// direct run buffers stdout locally and only does so while `sink_active()` is
/// `false`. A test that installs a sink therefore steals a concurrent
/// pipeline-running test's buffered stdout and is polluted by that test's
/// writes. Every test that installs a sink OR runs a pipeline (and so writes
/// through the same global stdout/stderr path) holds this lock, which keeps the
/// module parallel-safe against the rest of the suite without a
/// `--test-threads=1` fence.
pub fn lock_run_event_sink() -> MutexGuard<'static, ()> {
    RUN_EVENT_SINK_LOCK
        .get_or_init(|| Mutex::new(()))
        .blocking_lock()
}

/// Async variant for tests that hold the sink lock across `.await`.
pub async fn lock_run_event_sink_async() -> MutexGuard<'static, ()> {
    RUN_EVENT_SINK_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .await
}
