- **Pattern learning in read-only agent modes.** Learned-context lookup now skips
  legacy migration writes, and post-run observation degrades to explicit
  unavailable metadata when storage is denied, so read-only/headless agent runs
  no longer abort before the first model call.
