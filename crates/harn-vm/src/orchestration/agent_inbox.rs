//! Unified per-session inbox for asynchronous nudges.
//!
//! Any code path that needs to deliver a payload to a running agent loop
//! from *outside* the loop's current iteration funnels through this
//! module. That includes:
//!
//!   * Long-running tool completions (`stdlib::long_running`,
//!     `harn-hostlib::tools::run_command`'s background worker).
//!   * Command-policy post-hooks that produce feedback for the next turn
//!     (`orchestration::command_policy`).
//!   * MCP server `notifications/progress` messages relayed from the
//!     client transport.
//!   * Trigger/connector handlers that want to nudge a session that's
//!     already running (e.g. "the GitHub PR you've been waiting on just
//!     merged").
//!   * File-edited notifications queued by sync builtins like
//!     `write_file`.
//!
//! ## Lifecycle and ordering
//!
//! Each producer calls [`push`] with the target `session_id`, a `kind`
//! string (used as the synthetic message marker downstream), the
//! payload `content`, and a `source` label for telemetry. Entries land
//! in a per-session FIFO with monotonically increasing sequence
//! numbers. Consumers (the agent loop, `run_command`'s short-wait
//! helper, tests) call [`drain`] at well-defined boundaries:
//!
//!   1. Turn start, before `agent_autocompact_if_needed`. The summary
//!      then reflects events that piled up between turns.
//!   2. Turn start, *after* `agent_autocompact_if_needed`. Any push
//!      that landed *during* the LLM summarization call gets injected
//!      verbatim into the post-compaction transcript so it's visible
//!      in this turn's prompt.
//!
//! The "second drain after compaction" is the load-bearing piece — it
//! eliminates the race where async events arriving during a multi-
//! second compaction LLM call would otherwise wait an entire extra
//! turn before the agent sees them.
//!
//! ## Sync vs async waiters
//!
//! [`wait_sync`] blocks the calling thread on a Condvar paired with
//! the push side. It exists because a handful of sync builtins
//! (notably `run_command`'s "wait briefly for initial output" helper)
//! need to park before returning to the VM. The Condvar wait uses
//! wall-clock time — it is only appropriate inside an already-blocking
//! sync builtin.
//!
//! [`wait_async`] is the canonical async waiter. It composes a
//! `tokio::sync::Notify` with [`harn_clock::Clock`] so tests can
//! advance virtual time. New consumers should always reach for the
//! async variant.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use harn_clock::{Clock, RealClock};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// A single inbox entry. Producers fill in `kind`, `content`, and
/// `source`; the inbox stamps `sequence` and `ts_ms`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InboxEntry {
    /// Monotonically increasing per-session sequence number, starting
    /// at 1. Wraps at `u64::MAX` (unreachable in practice).
    pub sequence: u64,
    pub session_id: String,
    pub kind: String,
    pub content: String,
    pub source: String,
    pub ts_ms: i64,
}

#[derive(Default)]
struct InboxState {
    entries: VecDeque<InboxEntry>,
    seq: u64,
    notify: std::sync::Arc<Notify>,
}

struct InboxRegistry {
    inboxes: Mutex<HashMap<String, InboxState>>,
    sync_cv: Condvar,
}

impl InboxRegistry {
    fn new() -> Self {
        Self {
            inboxes: Mutex::new(HashMap::new()),
            sync_cv: Condvar::new(),
        }
    }
}

fn registry() -> &'static InboxRegistry {
    static REGISTRY: OnceLock<InboxRegistry> = OnceLock::new();
    REGISTRY.get_or_init(InboxRegistry::new)
}

fn lock_map(reg: &InboxRegistry) -> MutexGuard<'_, HashMap<String, InboxState>> {
    reg.inboxes.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Default clock used for inbox timestamps. Tests can install a paused
/// clock with [`install_clock`].
fn clock_arc() -> std::sync::Arc<dyn Clock> {
    static CLOCK: OnceLock<std::sync::Arc<dyn Clock>> = OnceLock::new();
    CLOCK
        .get_or_init(|| std::sync::Arc::new(RealClock::new()) as std::sync::Arc<dyn Clock>)
        .clone()
}

/// Install a clock implementation. Idempotent: only the first caller
/// wins (subsequent calls are silently dropped) which matches the
/// pattern used elsewhere in the runtime.
pub fn install_clock(clock: std::sync::Arc<dyn Clock>) {
    static SLOT: OnceLock<std::sync::Arc<dyn Clock>> = OnceLock::new();
    let _ = SLOT.set(clock);
}

/// Push an entry into `session_id`'s inbox and wake any waiters.
///
/// Safe to call from any thread. The session id may be empty — in that
/// case the entry lands in an unnamed bucket that legacy callers
/// (background fs/glob workers without a session context) can drain
/// with `drain("")`. New producers should always supply a session id.
pub fn push(session_id: &str, kind: &str, content: &str, source: &str) {
    let reg = registry();
    let notify = {
        let mut map = lock_map(reg);
        let state = map.entry(session_id.to_string()).or_default();
        state.seq = state.seq.wrapping_add(1).max(1);
        let entry = InboxEntry {
            sequence: state.seq,
            session_id: session_id.to_string(),
            kind: kind.to_string(),
            content: content.to_string(),
            source: source.to_string(),
            ts_ms: harn_clock::now_wall_ms(&*clock_arc()),
        };
        state.entries.push_back(entry);
        state.notify.clone()
    };
    reg.sync_cv.notify_all();
    notify.notify_waiters();
}

/// Drain every entry currently queued for `session_id`, in FIFO order.
pub fn drain(session_id: &str) -> Vec<InboxEntry> {
    let reg = registry();
    let mut map = lock_map(reg);
    map.get_mut(session_id)
        .map(|state| state.entries.drain(..).collect())
        .unwrap_or_default()
}

/// Drain entries from `session_id` whose `kind` matches `predicate`.
/// Non-matching entries are kept in the inbox in their original order.
pub fn drain_where<F>(session_id: &str, mut predicate: F) -> Vec<InboxEntry>
where
    F: FnMut(&InboxEntry) -> bool,
{
    let reg = registry();
    let mut map = lock_map(reg);
    let Some(state) = map.get_mut(session_id) else {
        return Vec::new();
    };
    let mut taken = Vec::new();
    let mut kept = VecDeque::with_capacity(state.entries.len());
    for entry in state.entries.drain(..) {
        if predicate(&entry) {
            taken.push(entry);
        } else {
            kept.push_back(entry);
        }
    }
    state.entries = kept;
    taken
}

/// Re-insert an entry at the front of `session_id`'s inbox. Used when a
/// consumer drains optimistically, peeks, and discovers the entry is
/// not relevant to its caller (e.g. `run_command` waiting for one
/// specific `handle_id`).
pub fn requeue_front(entry: InboxEntry) {
    let reg = registry();
    let mut map = lock_map(reg);
    let state = map.entry(entry.session_id.clone()).or_default();
    state.entries.push_front(entry);
}

/// Number of entries currently queued for `session_id`.
pub fn pending_count(session_id: &str) -> usize {
    let reg = registry();
    let map = lock_map(reg);
    map.get(session_id)
        .map(|state| state.entries.len())
        .unwrap_or(0)
}

/// Drop everything queued for `session_id`. Called when a session ends.
pub fn clear_session(session_id: &str) {
    let reg = registry();
    let mut map = lock_map(reg);
    map.remove(session_id);
}

/// Wipe every inbox. Test-only — production callers must use
/// [`clear_session`].
#[cfg(any(test, feature = "vm-bench-internals"))]
pub fn reset() {
    let reg = registry();
    let mut map = lock_map(reg);
    map.clear();
}

/// Sync wait. Parks the calling thread on a Condvar until `session_id`
/// has at least one queued entry, or `timeout` elapses. Returns `true`
/// if an entry is present at return time.
///
/// **When to use:** only inside sync builtins that are already blocking
/// the runtime thread (e.g. `run_command`'s "wait_ms" helper). Every
/// other consumer must use [`wait_async`].
pub fn wait_sync(session_id: &str, timeout: Duration) -> bool {
    let reg = registry();
    let mut map = match reg.inboxes.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if has_pending(&map, session_id) {
        return true;
    }
    let start = std::time::Instant::now();
    loop {
        let remaining = match timeout.checked_sub(start.elapsed()) {
            Some(remaining) if !remaining.is_zero() => remaining,
            _ => return has_pending(&map, session_id),
        };
        let (next_guard, wait_result) = match reg.sync_cv.wait_timeout(map, remaining) {
            Ok(pair) => pair,
            Err(poison) => {
                let pair = poison.into_inner();
                (pair.0, pair.1)
            }
        };
        map = next_guard;
        if has_pending(&map, session_id) {
            return true;
        }
        if wait_result.timed_out() {
            return false;
        }
    }
}

fn has_pending(map: &HashMap<String, InboxState>, session_id: &str) -> bool {
    map.get(session_id)
        .map(|s| !s.entries.is_empty())
        .unwrap_or(false)
}

/// Async wait. Returns when `session_id` has at least one queued entry
/// or `clock.sleep(timeout)` resolves. Uses `tokio::sync::Notify` so it
/// composes correctly with `tokio::time::pause()` and the
/// [`harn_clock::PausedClock`] used by deterministic tests.
pub async fn wait_async(session_id: &str, timeout: Duration, clock: &dyn Clock) -> bool {
    if pending_count(session_id) > 0 {
        return true;
    }
    let notify = {
        let reg = registry();
        let mut map = lock_map(reg);
        map.entry(session_id.to_string())
            .or_default()
            .notify
            .clone()
    };
    let sleep = clock.sleep(timeout);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            biased;
            _ = notify.notified() => {
                if pending_count(session_id) > 0 {
                    return true;
                }
            }
            () = &mut sleep => {
                return pending_count(session_id) > 0;
            }
        }
    }
}

/// Snapshot a session's queue without draining. Test/observability
/// helper.
#[cfg(any(test, feature = "vm-bench-internals"))]
pub fn snapshot(session_id: &str) -> Vec<InboxEntry> {
    let reg = registry();
    let map = lock_map(reg);
    map.get(session_id)
        .map(|state| state.entries.iter().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_clock::PausedClock;
    use time::OffsetDateTime;

    fn fresh_session_id() -> String {
        // Each test owns its own session id, so the global registry
        // doesn't need per-test wipes; that also keeps concurrent
        // cargo-nextest runs isolated from each other.
        format!("test-{}", uuid::Uuid::now_v7())
    }

    #[test]
    fn push_then_drain_preserves_fifo_order() {
        let sid = fresh_session_id();
        push(&sid, "tool_result", "first", "test");
        push(&sid, "tool_result", "second", "test");
        push(&sid, "file_edited", "third", "test");
        let entries = drain(&sid);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content, "first");
        assert_eq!(entries[1].content, "second");
        assert_eq!(entries[2].content, "third");
        assert!(entries[0].sequence < entries[1].sequence);
        assert!(entries[1].sequence < entries[2].sequence);
    }

    #[test]
    fn drain_where_partitions_by_kind() {
        let sid = fresh_session_id();
        push(&sid, "tool_result", "a", "test");
        push(&sid, "file_edited", "b", "test");
        push(&sid, "tool_result", "c", "test");
        let taken = drain_where(&sid, |e| e.kind == "file_edited");
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].content, "b");
        let remaining = drain(&sid);
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].content, "a");
        assert_eq!(remaining[1].content, "c");
    }

    #[test]
    fn requeue_front_keeps_unwanted_entry_at_head() {
        let sid = fresh_session_id();
        push(&sid, "tool_result", "first", "test");
        let mut entries = drain(&sid);
        assert_eq!(entries.len(), 1);
        let entry = entries.remove(0);
        requeue_front(entry);
        let again = drain(&sid);
        assert_eq!(again[0].content, "first");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn wait_async_returns_when_push_happens() {
        let sid = fresh_session_id();
        let clock = PausedClock::new(OffsetDateTime::UNIX_EPOCH);
        let waiter_sid = sid.clone();
        let waiter_clock = clock.clone();
        let waiter = tokio::spawn(async move {
            wait_async(&waiter_sid, Duration::from_secs(60), &*waiter_clock).await
        });
        // Yield once so the waiter installs its notify watch before we push.
        tokio::task::yield_now().await;
        push(&sid, "tool_result", "hello", "test");
        assert!(waiter.await.expect("join"));
        // Push side wrote one entry — verify it's still drainable.
        let entries = drain(&sid);
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn wait_async_times_out_when_silent() {
        let sid = fresh_session_id();
        let clock = PausedClock::new(OffsetDateTime::UNIX_EPOCH);
        // Drive the timeout to completion by advancing logical time.
        let clock_advance = clock.clone();
        let advancer = tokio::spawn(async move {
            tokio::task::yield_now().await;
            clock_advance.advance(Duration::from_millis(50));
        });
        let result = wait_async(&sid, Duration::from_millis(50), &*clock).await;
        advancer.await.ok();
        assert!(!result);
    }

    #[test]
    fn pending_count_tracks_pushes_and_drains() {
        let sid = fresh_session_id();
        assert_eq!(pending_count(&sid), 0);
        push(&sid, "tool_result", "x", "test");
        push(&sid, "tool_result", "y", "test");
        assert_eq!(pending_count(&sid), 2);
        let _ = drain(&sid);
        assert_eq!(pending_count(&sid), 0);
    }

    #[test]
    fn clear_session_drops_pending_entries() {
        let sid = fresh_session_id();
        push(&sid, "tool_result", "x", "test");
        clear_session(&sid);
        assert_eq!(pending_count(&sid), 0);
    }
}
