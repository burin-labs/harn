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

### `harn_clock::Clock` injection (preferred for runtime code)

The unified `harn_clock::Clock` trait is the canonical way to read time and
sleep in Harn runtime code. Cron, the trigger dispatcher, the stdlib
`now_ms` / `monotonic_ms` / `sleep_ms` builtins, the `OrchestratorHarness`,
and (via re-export) every downstream crate route through it.

```rust
use std::sync::Arc;
use std::time::Duration;
use harn_clock::Clock;

struct Worker {
    clock: Arc<dyn Clock>,
}

impl Worker {
    async fn poll_until(&self, deadline_ms: i64) {
        while self.clock.now_utc().unix_timestamp_nanos() / 1_000_000 < deadline_ms {
            self.clock.sleep(Duration::from_millis(50)).await;
        }
    }
}
```

Tests substitute `harn_clock::PausedClock` to drive virtual time
deterministically — see the `MockClock` section below for the existing trigger
test surface or use `PausedClock` directly:

```rust
use harn_clock::{Clock, PausedClock};

#[tokio::test]
async fn worker_resumes_after_advance() {
    let clock = PausedClock::new(time::OffsetDateTime::now_utc());
    let worker = Worker { clock: clock.clone() as Arc<dyn Clock> };
    let task = tokio::spawn(async move { worker.poll_until(/* future ms */ ...).await });
    clock.advance(Duration::from_secs(60));
    task.await.unwrap();
}
```

`PausedClock` works in both `current_thread` and `multi_thread` runtimes and
does not require `start_paused = true`. `RecordedClock` wraps any inner clock
and captures every observation to a `ClockEventLog`, which is the substrate
the recording/replay child issue (#1441) builds on.

For runtime code that needs `tokio::time::sleep` directly (e.g. `timeout`),
combine `PausedClock` with `tokio::time::pause()` so both surfaces freeze
together.

The lint at `scripts/lint_test_patterns.sh` forbids new wall-clock reads in
non-test files under `crates/harn-vm/src/` and `crates/harn-cli/src/` outside
the explicit `NON_TEST_WALL_CLOCK_ALLOWLIST`. The allowlist freezes existing
sites as gradual cleanup; new files must accept `Arc<dyn Clock>` instead of
calling `OffsetDateTime::now_utc()` / `Instant::now()` directly.

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

Pass a custom clock via `OrchestratorConfig::with_clock(...)`:

```rust
let clock = harn_vm::clock::PausedClock::new(time::OffsetDateTime::now_utc());
let config = OrchestratorConfig::for_test(manifest, state_dir).with_clock(clock.clone());
let harness = OrchestratorHarness::start(config).await?;
clock.advance(Duration::from_secs(60));
```

Cron and trigger-dispatch logic inside the harness then run on the injected
virtual clock.

### `Harness::null()` and `Harness::mock()`

For VM tests that exercise `fn main(harness: Harness)` entrypoints directly,
prefer the test-mode harness constructors over ambient host access.

Use `Harness::null()` for sandbox-violation tests. It denies every sub-handle
method and records the typed deny event so the test can assert the exact
capability surface the script tried to use:

```rust
let harness = harn_vm::Harness::null();
vm.set_harness(harness.clone());
let error = vm.execute(&chunk).await.expect_err("capability denied");

let events = harness.deny_events();
assert_eq!(events[0].sub_handle, harn_vm::HarnessKind::Fs);
assert_eq!(events[0].method, "read_text");
assert_eq!(events[0].args, ["/secrets"]);
```

Use `Harness::mock()` for deterministic happy-path tests. The builder installs
a paused clock and canned responses; calls are recorded for assertions after
the VM run:

```rust
let harness = harn_vm::Harness::mock()
    .clock_at_unix_ms(1_700_000_000_000)
    .env("KEY", "value")
    .fs_read("/x", b"data".to_vec())
    .random_u64(42)
    .net_get("https://example.test", "body")
    .build();

vm.set_harness(harness.clone());
vm.execute(&chunk).await?;

assert_eq!(harness.captured_stdio(), "ok\n");
assert_eq!(harness.calls()[0].sub_handle, harn_vm::HarnessKind::Stdio);
```

Conformance fixtures can opt into these handles with an adjacent
`<name>.harness.json` sidecar. Keep the sidecar small: choose `"mode": "null"`
or `"mode": "mock"`, provide only the canned responses needed by that fixture,
and assert the recorded calls or deny events there.

```json
{
  "mode": "mock",
  "clock_at_unix_ms": 1700000000000,
  "env": {"KEY": "value"},
  "fs_reads": {"/x": "data"},
  "random_u64": [42],
  "net_gets": {"https://example.test": "body"},
  "expect_calls": [
    {"sub_handle": "env", "method": "get", "args": ["KEY"]}
  ]
}
```

### `MockProcess`

For subprocess tests that do not need real shell behavior, use `MockProcess`.
It exposes a synchronous control channel so the test drives process state
(exit code, stdout lines, signal receipt) without polling.

### Unified `mock_time(...)` for Harn fixtures and stdlib builtins

Conformance fixtures and Rust-side tests that exercise stdlib timing
builtins (`sleep`, `sleep_ms`, `now_ms`, `monotonic_ms`, `timestamp`,
`elapsed`, `command_step` retry) all share one mock-clock stack
(`harn_vm::clock_mock`). This same stack also drives the trigger
dispatcher and the cron scheduler — installing one mock pins time
everywhere a Harn script, a connector, or a Rust test would otherwise
read it.

```harn
pipeline test(task) {
  mock_time(1700000000000)
  // sleep advances the mock; no wall-clock burn, no scheduler races.
  sleep(50ms)
  log(now_ms())          // 1700000000050
  advance_time(1000)
  log(monotonic_ms())    // 1050
  // yield_now lets sibling parallel-each tasks make progress without
  // advancing time at all.
  yield_now()
  unmock_time()
}
```

Rust tests can install the same mock through `stdlib::clock::MockClockGuard`
or `clock_mock::install_override(MockClock::new(...))` — both push onto
the same thread-local stack, so a stdlib-side guard is observed by the
trigger dispatcher and vice versa.

Fixtures that genuinely need wall-clock time (real subprocess I/O,
real socket-bound servers, scheduler tests timing real backoffs) are
exempt via `CONFORMANCE_REAL_TIME_ALLOWLIST` in
`scripts/lint_test_patterns.sh`. The lint catches new fixtures that
sleep on a literal duration without either entering a `mock_time(...)`
block or being added to the allowlist with reviewer justification.

## Forbidden patterns

The following patterns are banned in test files by `make lint-test-patterns`.
The script searches files under `crates/**/tests/**/*.rs`,
`crates/**/src/**/tests.rs`, `crates/**/src/**/tests_*.rs`, and
`conformance/tests/**/*.harn`.

| Pattern | Why it is banned | Approved alternative |
|---|---|---|
| `std::thread::sleep(` | Blocks the thread, races against scheduler | `tokio::time::pause()` + `advance()` |
| `tokio::time::sleep(` (outside `start_paused`) | Non-deterministic; races against system load | `start_paused = true` + `advance()` |
| `while … Instant::now()` | Wall-clock polling loop; flaky under load | `EventLog::subscribe()` + `timeout` |
| `SystemTime::now()` in tests | Real wall-clock timestamp; non-reproducible | `MockClock` or injected timestamp |
| `recv_timeout(Duration::from_millis(…))` | Busy-wait with a short literal timeout | `tokio::time::timeout` with event channel |
| `#[ignore]` outside slow harn-cli integration tests | Hides regressions behind default-suite skips | run the test by default, or move subprocess coverage to the harn-cli E2E profile |
| copied conformance subprocess wait helpers | Drifts retry ceilings and diagnostics between fixtures | import `conformance/tests/_common.harn` |
| `random_int(20000, 45000)` for server ports | Races with other tests and local services | bind port `0` and read the readiness log |
| `sleep(<literal>)` / `time.sleep(<literal>)` in `.harn` fixtures (outside `mock_time(...)`) | Wall-clock burn that races against scheduler load | wrap in `mock_time(...)` / `unmock_time()` and let the unified clock auto-advance, or add the file to `CONFORMANCE_REAL_TIME_ALLOWLIST` with justification |

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
- Use named constants colocated with the E2E module rather than inline
  `Duration::from_millis(…)` literals so timeout values are easy to audit and
  tune.
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
