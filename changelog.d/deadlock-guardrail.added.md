- **The VM now detects deterministic self-deadlocks instead of hanging
  forever (HARN-ORC-011).** Re-entering a `mutex { … }` block on a lock this
  task already holds — directly or through a called function — and `await`ing
  a task's own join handle previously blocked the VM indefinitely with no
  diagnostic. Both now raise a clear, catchable error before blocking. Run
  `harn explain HARN-ORC-011` for guidance. (Cross-task wait-for-graph and
  builtin-`sync_*`-path detection remain follow-ups.)
