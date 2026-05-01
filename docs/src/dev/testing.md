# Deterministic test patterns

This page documents how to write fast, deterministic tests in the Harn
workspace. It explains the approved patterns, the patterns that are banned by
`make lint-test-patterns`, how to opt out when a ban is unavoidable, and how to
write tests that need real subprocesses.

## Background

A multi-tier deflake effort ([#1057]) removed wall-clock polling from the fast
test suite. Before that work, many unit and integration tests used patterns like
`tokio::time::sleep(Duration::from_millis(50))` or polling loops driven by
`Instant::now()`. These patterns caused the suite to be sensitive to scheduler
jitter and system load, and were the primary source of intermittent failures on
CI and slow developer machines.

The lint at `scripts/lint_test_patterns.sh` (run by `make lint-test-patterns`)
enforces that new test code does not reintroduce these patterns.

[#1057]: https://github.com/burin-labs/harn/issues/1057

## Approved patterns

### `tokio::time::pause()` and `advance()`

For tests that need to simulate time passing, use Tokio's paused-time runtime.
A test annotated with `start_paused = true` starts with the clock frozen at an
arbitrary epoch and advances only when you call `tokio::time::advance()`.

```rust
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_fires_after_deadline() {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = tx.send(());
    });

    // Advance 6 seconds in zero wall-clock time.
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(rx.await.is_ok());
}
```

**Caveats:**

- `start_paused = true` only works with `flavor = "current_thread"`. The
  multi-thread runtime shares a real monotonic clock and cannot be paused.
- `tokio::time::advance()` only drives Tokio timers (`sleep`, `timeout`,
  `interval`). It does not advance `SystemTime::now()`, `Instant::now()`, or
  any file-descriptor-backed timer. If your code mixes Tokio timers with
  wall-clock reads, both need injection.
- Do not mix `start_paused = true` tests with code that touches real I/O
  (network, file system). The paused runtime will not drive completion events
  from the OS while time is frozen; a real TCP write behind a `tokio::time::sleep`
  may never complete.

### `EventLog::subscribe()`

For tests that wait for something to happen inside a running component,
subscribe to its `EventLog` and block on the channel with a `tokio::time::timeout`
ceiling.

```rust
let (log, handle) = EventLog::new();
let mut sub = log.subscribe("trigger.outbox").await;

// Trigger the action under test.
component.do_thing().await;

// Wait for the expected event — hard fail-fast after 5 s.
let event = tokio::time::timeout(Duration::from_secs(5), sub.recv())
    .await
    .expect("timed out waiting for trigger.outbox event")
    .expect("channel closed");

assert_eq!(event.kind, "dispatch");
```

The `tokio::time::timeout` here is the right pattern: it is a hard ceiling that
turns a hang into a fast failure. Pair it with a meaningful error message so the
failure is obvious.

### `OrchestratorHarness`

For tests that need the orchestrator running but do not need real subprocesses,
use `OrchestratorHarness` from the test-util crate. It boots the orchestrator
in-process with an injectable clock and exposes event subscriptions so tests can
wait deterministically.

### `MockProcess`

For subprocess tests that do not need real shell behavior, use `MockProcess`.
It exposes a synchronous control channel so the test drives process state
(exit code, stdout lines, signal receipt) without polling.

## Forbidden patterns

The following patterns are banned in test files by `make lint-test-patterns`.
The script searches files under `crates/**/tests/**/*.rs`,
`crates/**/src/**/tests.rs`, and `crates/**/src/**/tests_*.rs`.

| Pattern | Why it is banned | Approved alternative |
|---|---|---|
| `std::thread::sleep(` | Blocks the thread, races against scheduler | `tokio::time::pause()` + `advance()` |
| `tokio::time::sleep(` (outside `start_paused`) | Non-deterministic; races against system load | `start_paused = true` + `advance()` |
| `while … Instant::now()` | Wall-clock polling loop; flaky under load | `EventLog::subscribe()` + `timeout` |
| `SystemTime::now()` in tests | Real wall-clock timestamp; non-reproducible | `MockClock` or injected timestamp |
| `recv_timeout(Duration::from_millis(…))` | Busy-wait with a short literal timeout | `tokio::time::timeout` with event channel |

## Opting out

If you are writing a test that genuinely cannot use any of the approved
patterns — typically because it exercises real subprocess I/O or a syscall that
has no deterministic equivalent — you have two options:

1. **Move the test to the slow E2E suite** (see below). Subprocess tests belong
   in files named `*_e2e.rs` or under `tests/` directories that are not part
   of the fast nextest run.

2. **Add the file to the per-pattern allowlist in `scripts/lint_test_patterns.sh`.**
   Open a PR that adds your file to the appropriate array (`THREAD_SLEEP_ALLOWLIST`,
   `TOKIO_SLEEP_ALLOWLIST`, etc.), includes a one-line comment in the array entry
   explaining why the opt-out is justified, and gets a second reviewer sign-off.
   The allowlist is public and tracked as technical debt; entries are expected to
   shrink, not grow, as the codebase matures.

## Writing subprocess tests in the slow E2E suite

Real subprocess tests — those that spawn `harn` as a child process, send signals,
or read real file output — belong in files ending `_e2e.rs` or under the
`crates/harn-cli/tests/` tree that is excluded from the sub-second nextest profile.

These tests are subject to different rules:

- Wall-clock timeouts (`Instant::now()` deadlines, `recv_timeout`) are acceptable
  because there is no deterministic alternative for real process I/O.
- Use named constants (`EVENT_FAIL_FAST_TIMEOUT`, `PROCESS_FAIL_FAST_TIMEOUT`)
  rather than inline `Duration::from_millis(…)` literals so timeout values are
  easy to audit and tune centrally.
- Always provide a human-readable timeout message so a failure says _what_ timed
  out, not just that an assertion failed.
- Prefer `tokio::time::timeout` over `recv_timeout` even in E2E tests; it
  composes better with async code and gives cleaner error messages.

## Using `tokio::time::pause()` — common mistakes

### Multi-thread flavor

```rust
// WRONG — start_paused only works with current_thread.
#[tokio::test(flavor = "multi_thread", start_paused = true)]
async fn broken() { … }
```

Use `flavor = "current_thread"` for paused-time tests.

### Real I/O behind a Tokio timer

```rust
// WRONG — the TCP read will never complete while time is paused.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn broken() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await; // pause doesn't drive I/O
    let _ = listener.accept().await; // hangs
}
```

If your test needs both time control and real I/O, use the multi-thread runtime
and a `tokio::time::timeout` ceiling instead of `start_paused`.

### `advance()` semantics

`tokio::time::advance(d)` adds `d` to the Tokio clock and polls all pending
timers that would fire within that window. It does not yield to other tasks
automatically; if the task that sets a timer has not yet been polled to
register it, `advance()` may appear to do nothing.

The fix is to yield once before advancing:

```rust
tokio::task::yield_now().await;
tokio::time::advance(Duration::from_secs(1)).await;
```
