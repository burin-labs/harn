use std::time::Duration;

/// Poll cadence for short test-only fallback loops.
pub const FILE_WATCH_FALLBACK_POLL: Duration = Duration::from_millis(10);
/// Grace period for negative assertions after shutdown or process cancellation.
pub const PROCESS_EXIT_GRACE: Duration = Duration::from_millis(100);
/// Initial wait/poll cadence for tests that probe asynchronous network dispatch.
pub const NETWORK_PROBE_INITIAL: Duration = Duration::from_millis(20);
/// Default upper bound for trigger test harness waits.
pub const TEST_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
