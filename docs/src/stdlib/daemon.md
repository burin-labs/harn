# Daemon stdlib

Harn's daemon builtins wrap the existing `agent_loop(harness, ..., {daemon: true})`
runtime so scripts can manage long-lived assistants without hand-assembling
snapshot paths and resume options.

Daemon idle is implemented as a special case of `agent_await_resumption` with
`timeout` / `on_event` resume conditions preconfigured from daemon options. The
runtime still waits in-process at the idle boundary, so existing daemon loops
keep their `active -> idle -> active` behavior instead of returning a suspended
handle to the caller.

## Builtins

### `harness.agent.daemon_spawn(config)`

Start a daemon-mode agent and return a daemon handle dict.

Required config:

- `task`
- `persist_path`

Useful optional config:

- `name`
- `system`
- `provider`, `model`, `tools`, `max_iterations`, and other `agent_loop` options
- `wake_interval_ms`
- `watch_paths`
- `idle_watchdog_attempts`
- `event_queue_capacity` (default `1024`)

Example:

```harn
fn main(harness: Harness) {
  const reviewer = harness.agent.daemon_spawn({
    name: "reviewer",
    task: "Watch for trigger events and summarize the change.",
    system: "You are a careful code reviewer.",
    provider: "mock",
    persist_path: ".harn/daemons/reviewer",
    watch_paths: ["src/"],
    wake_interval_ms: 30000,
    event_queue_capacity: 256,
  })
}
```

### `harness.agent.daemon_trigger(handle, event)`

Queue a trigger event for a running daemon. Events are delivered FIFO, one
daemon wake at a time, and the queue is durably persisted in the daemon's
metadata so a stop/resume or crash/recovery cycle does not lose pending work.

If the queue is full, the builtin throws `VmError::DaemonQueueFull`.

```harn
fn wake(harness: Harness, reviewer) {
  harness.agent.daemon_trigger(reviewer, {
    kind: "file_changed",
    path: "src/lib.rs",
  })
}
```

### `harness.agent.managed_daemon_snapshot(handle)`

Return the latest persisted daemon snapshot plus live queue metadata:

- `pending_events`
- `pending_event_count`
- `inflight_event`
- `queued_event_count`
- `event_queue_capacity`

The rest of the payload mirrors `agent_loop` daemon snapshots, including
`daemon_state`, `recorded_messages`, `total_iterations`, and `saved_at`.

### `harness.agent.managed_daemon_wait(handle, min_iterations?, timeout_ms?)`

Wait until the daemon is idle, its trigger queue is empty, and its persisted
snapshot has reached `min_iterations`. The defaults are `0` iterations and a
5,000 ms timeout.

The builtin returns the same closed snapshot record as
`managed_daemon_snapshot`. It throws if the daemon stops, fails, or doesn't
meet all three conditions before the timeout. Use it after `daemon_trigger`
when later work depends on the trigger's completed turn:

```harn
const before = harness.agent.managed_daemon_wait(reviewer)
harness.agent.daemon_trigger(reviewer, {
  kind: "file_changed",
  path: "src/lib.rs",
})
const after = harness.agent.managed_daemon_wait(
  reviewer,
  before.total_iterations + 1,
)
```

### `harness.agent.daemon_stop(handle)`

Stop a daemon and preserve its state on disk. The runtime waits briefly for an
idle boundary when possible; if the daemon is still mid-turn, the current
in-flight trigger is re-queued so `harness.agent.daemon_resume(...)` can
replay it safely.

### `harness.agent.daemon_resume(path)`

Resume a daemon from its persisted state directory. The path is the same root
directory you passed as `persist_path` to `harness.agent.daemon_spawn(...)`,
not the inner
`daemon.json` snapshot file.

If the daemon stopped with queued or in-flight trigger events, they are restored
and replayed after resume.

## Delivery semantics

- Trigger events are FIFO.
- The queue is bounded by `event_queue_capacity`.
- Trigger payloads are handed to the daemon only from an idle boundary, so a
  persisted snapshot always reflects the pre-trigger or post-trigger state and
  never an ambiguous half-consumed queue.
- `managed_daemon_wait` observes idle transitions directly. It doesn't poll
  the daemon or require a caller-owned sleep loop.
- Forced stop/restart is intentionally at-least-once: an in-flight trigger is
  re-queued on stop/resume instead of being dropped silently.
