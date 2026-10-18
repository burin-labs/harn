- **Bounded decision calls can accompany ordinary agent work.** An explicit per-call evaluation limit no longer requires
  changing the budget mode for the entire coding session. Decisions still respect the parent budget, and a provider
  exceeding its admitted bound stops subsequent paid calls (#10179).
- **Optional classification failures preserve compaction progress.** Before any classification succeeds, an unavailable
  helper falls back to observation masking. After partial progress, compaction retains the remaining context and reports
  a deferred result. Cancellation and session-wide budget exhaustion still stop the operation (#10179).
