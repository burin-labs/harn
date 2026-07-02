- **Release audit tests are deterministic under parallel load.** Harn VM
  waitpoint and action-graph regression tests now filter persisted event-log
  records by the run or waitpoint they created instead of depending on shared
  topic ordering or thread-local test signals.
