- Process tool cleanup receipts now report structural child-process evidence.
  Timeout, interrupt, and long-running cancellation paths carry a bounded
  `process_cleanup` receipt with observed descendants, reaped children,
  survivor counts, and basename-only child command names.
