- Fixed the code-index symbol graph accumulating duplicate Module→Module
  `IMPORTS` edges on every incremental reindex. `link_imports` re-runs over the
  whole workspace after each per-file reindex, but `rebuild_file` only clears the
  reindexed file's edges, so every reindex appended another copy of every
  still-valid import edge between unchanged files — despite the documented
  "idempotent" / "add-only" contract. The edge set grew without bound and Cypher
  `IMPORTS`/`IMPORTED_BY` traversals returned duplicate rows, wasting the row
  budget and polluting code-index grounding. The relink is now idempotent.
