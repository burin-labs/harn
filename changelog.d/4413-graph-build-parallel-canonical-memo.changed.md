- Module-graph construction now loads each import wave on a parallel worker
  pool (`HARN_MODULE_GRAPH_JOBS=<n>` pins it; `1` restores the serial walk),
  path canonicalization is memoized process-wide, and `harn check` loads the
  per-directory `[check]` config once per directory instead of once per file.
  Together these cut the syscall-bound remainder of whole-tree `harn check`
  after the parallel-driver work landed.
