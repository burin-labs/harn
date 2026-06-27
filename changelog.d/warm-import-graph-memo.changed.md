- Memoized the per-file read+import-scan and `canonicalize` work inside the
  bytecode cache's transitive import-graph hash (`CacheKey::from_source`). A cold
  `harn run` over a large pipeline calls `from_source` once per module load — the
  Burin 286-file pipeline does ~175 of them — and each call previously re-read,
  re-scanned, and re-`realpath`'d every shared library file on the import graph.
  The walk now reads and canonicalizes each file at most once per stat identity,
  so the import-graph hash drops from ~3.6s to ~0.4s on a warm process and the
  whole pre-execution module-load phase falls from ~10s to ~0.6s steady-state
  (~2x faster even on a single-shot cold process). The memo is keyed by
  `(path, len, mtime_ns)`, so any on-disk edit busts it and a long-lived warm
  process still recompiles edited pipelines correctly; the folded hash bytes are
  byte-identical to the un-memoized path, so cache keys are unchanged.
