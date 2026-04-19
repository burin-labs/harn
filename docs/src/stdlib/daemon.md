# Daemon stdlib

Harn's daemon builtins wrap the existing `agent_loop(..., {daemon: true})`
runtime so scripts can manage long-lived assistants without hand-assembling
snapshot paths and resume options.

## Builtins

### `daemon_spawn(config)`

Start a daemon-mode agent and return a daemon handle dict.

Required config:

- `task` or `prompt`
- `persist_path` or `state_dir`

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
let reviewer = daemon_spawn({
  name: "reviewer",
  task: "Watch for trigger events and summarize the change.",
  system: "You are a careful code reviewer.",
  provider: "mock",
  persist_path: ".harn/daemons/reviewer",
  watch_paths: ["src/"],
  wake_interval_ms: 30000,
  event_queue_capacity: 256,
})
```

### `daemon_trigger(handle, event)`

Queue a trigger event for a running daemon. Events are delivered FIFO, one
daemon wake at a time, and the queue is durably persisted in the daemon's
metadata so a stop/resume or crash/recovery cycle does not lose pending work.

If the queue is full, the builtin throws `VmError::DaemonQueueFull`.

```harn
daemon_trigger(reviewer, {
  kind: "file_changed",
  path: "src/lib.rs",
})
```

### `daemon_snapshot(handle)`

Return the latest persisted daemon snapshot plus live queue metadata:

- `pending_events`
- `pending_event_count`
- `inflight_event`
- `queued_event_count`
- `event_queue_capacity`

The rest of the payload mirrors `agent_loop` daemon snapshots, including
`daemon_state`, `recorded_messages`, `total_iterations`, and `saved_at`.

### `daemon_stop(handle)`

Stop a daemon and preserve its state on disk. The runtime waits briefly for an
idle boundary when possible; if the daemon is still mid-turn, the current
in-flight trigger is re-queued so `daemon_resume(...)` can replay it safely.

### `daemon_resume(path)`

Resume a daemon from its persisted state directory. The path is the same root
directory you passed as `persist_path` / `state_dir` to `daemon_spawn(...)`,
not the inner `daemon.json` snapshot file.

If the daemon stopped with queued or in-flight trigger events, they are restored
and replayed after resume.

## Delivery semantics

- Trigger events are FIFO.
- The queue is bounded by `event_queue_capacity`.
- Trigger payloads are handed to the daemon only from an idle boundary, so a
  persisted snapshot always reflects the pre-trigger or post-trigger state and
  never an ambiguous half-consumed queue.
- Forced stop/restart is intentionally at-least-once: an in-flight trigger is
  re-queued on stop/resume instead of being dropped silently.
