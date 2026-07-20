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
///
/// The canonical failure this prevents: `playground.rs`'s
/// `playground_replays_cli_llm_mock_error_envelopes` runs a pipeline but for a
/// while held only `env_lock`, not this lock. Its `llm_call_safe`
/// error-envelope prints (`false/503/transient/…`) emitted as `Stdout` events
/// into a concurrent `--json` test's NDJSON buffer — a cross-test bleed that
/// failed under parallel `cargo test` yet passed under `--test-threads=1`. A
/// test touching both process globals takes both locks, and to stay
/// deadlock-free every such test acquires them in one fixed order: `env_lock`
/// first, then this lock.
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
