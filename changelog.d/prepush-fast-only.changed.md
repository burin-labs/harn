- **Pre-push hooks can now run in fast-only mode.** Set
  `HARN_PREPUSH_FAST_ONLY=1` to keep signature, merge-queue, and cheap drift
  guards while deferring expensive local build checks to remote CI.
