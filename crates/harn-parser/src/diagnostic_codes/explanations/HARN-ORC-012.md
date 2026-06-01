# HARN-ORC-012 — a wait-for graph cycle would block forever

## What it means

The VM detected that every active task in the current execution tree is waiting,
and the outstanding channel operations cannot match a sender with a receiver.
Without this guard the run would block forever.

This check only fires when the runtime can prove the wait is closed-world:
running tasks, sleeping tasks, time-bounded selects, and `deadline { ... }`
scopes keep the guard from reporting a deadlock.

## How to fix

Make sure every blocking `receive` has a task that can still send to that
channel, and every blocking `send` on a full channel has a task that can still
receive from it. If the wait is intentionally optional, use `try_receive`,
`select timeout`, `channel_select(..., timeout_ms)`, or wrap the operation in a
`deadline { ... }` block.

This error is non-retryable: it indicates a structural concurrency bug, not a
transient failure.
