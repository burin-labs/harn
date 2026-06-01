# HARN-ORC-011 — a self-deadlock acquire would block forever

## What it means

The VM detected an acquire that can never succeed and would otherwise block
forever, so it surfaced an error instead of hanging. Two deterministic,
provably-unresolvable cases are caught:

- **Re-entrant mutex.** A lexical `mutex { ... }` block acquires a
  non-reentrant, capacity-1 lock. Entering a second `mutex` block on the same
  key while the first is still held — directly or through a called function —
  can never be granted, because the sole permit holder is the task that is now
  waiting on it.
- **Self-join.** A task `await`s its own join handle. The handle only resolves
  when the task finishes, so waiting on it from inside the task itself can
  never complete.

Harn's static checks (like Rust's borrow checker) prevent data races but not
deadlocks; this runtime guard is the analogue of the Go runtime's
"all goroutines are asleep — deadlock!" detection for the cases that are
provable with no false positives.

## How to fix

- Do not nest `mutex { ... }` blocks that resolve to the same lock, and avoid
  calling a function that opens a `mutex` block from inside another one.
  Restructure so the lock is released before it is acquired again, or hold a
  single lock around the whole critical section.
- Never `await` the handle of the current task. Await it from the task that
  spawned it instead.

This error is non-retryable: it indicates a structural concurrency bug, not a
transient failure.
