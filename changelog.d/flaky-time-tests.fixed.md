- De-flaked and de-slowed several tests that relied on wall-clock timing or
  process-global state. The `ResourceGate` scheduler tests now assert gate
  state in-process via a non-blocking `try_acquire` instead of
  `thread::sleep`-coaxed thread ordering, the worker-snapshot round-trip test
  uses an explicit path instead of mutating the global `HARN_WORKER_STATE_DIR`
  env var, and diagnostic color in tests is forced off via a thread-local
  override instead of the process-global `NO_COLOR` env var.
