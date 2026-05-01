use std::time::Duration;

/// Poll cadence for short test-only fallback loops on real OS resources
/// (e.g. polling a non-blocking `TcpListener` for shutdown). Not for
/// dispatcher logic waits — those must be event-driven and run under
/// `tokio::test(start_paused = true)`.
pub const FILE_WATCH_FALLBACK_POLL: Duration = Duration::from_millis(10);
/// Grace period for negative assertions after shutdown or process cancellation
/// against real network mocks (TCP). Inside `start_paused` tests, prefer
/// `tokio::time::advance` instead.
pub const PROCESS_EXIT_GRACE: Duration = Duration::from_millis(100);
/// Hard fail-fast ceiling for trigger test harness waits.
///
/// Used as the upper bound on `tokio::time::timeout` calls and `Dispatcher::drain`
/// — a deterministic test should never come close to this. Under
/// `tokio::test(start_paused = true)`, paused-time auto-advance ensures the
/// ceiling fires immediately when no work remains, instead of burning real
/// wall-clock seconds. The 30-second value remains generous enough for
/// real-network A2A fixture tests.
pub const TEST_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
