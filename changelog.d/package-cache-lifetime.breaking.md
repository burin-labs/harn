- **Package cache reads now keep their source locked until copying or inspection
  finishes.** Cache cleanup waits for those readers, and Git and archive entries
  use a separate `v2` directory so older Harn processes cannot delete them.

  Migration: if the required packages exist only in the older cache layout,
  fetch them once online before installing a fresh project offline. Existing
  installed generations still work when their packages match the lock. Keep
  `HARN_CACHE_DIR` set to the same cache root as before.
