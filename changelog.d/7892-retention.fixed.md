- **The shared per-worktree Cargo target cache now has a size bound, not only
  an orphan reaper (#7892).** Dropping orphans bounds nothing on a machine
  whose worktrees are all alive: every live worktree kept its tree forever, so
  the cache grew with the number of worktrees the machine had ever had at once.
  Warm trees are now ranked by recency within their root; the newest
  `HARN_TARGET_GC_KEEP_RECENT` (default 10) are the working set and are kept
  whatever their age, and the rest are retired once nothing has built them for
  `HARN_TARGET_GC_MAX_IDLE_SECS` (default three days). A tree a live process is
  holding, and a tree whose mtime this run could not read, never reach the
  ranking.
