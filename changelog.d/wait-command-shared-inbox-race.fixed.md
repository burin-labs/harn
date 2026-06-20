- **`wait_command` no longer falsely reports a still-running handle when a
  sibling background command completes during the wait.** The per-`handle_id`
  wait is layered on a per-`session_id` completion inbox, and concurrent
  background handles can share one inbox bucket (notably the empty session id
  under `harn test`/headless). The old code parked once, re-drained once, and
  requeued any foreign sibling completion — falsely returning `status:
  "running"` for the handle the caller asked about, which then got cancelled.
  `wait_command::handle` now loops until either its own handle's result arrives
  or the timeout deadline elapses, re-parking for the remaining budget after
  each foreign wakeup. The `timeout_ms == 0` non-blocking poll is unchanged.
