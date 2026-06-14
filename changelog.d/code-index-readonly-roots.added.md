- **`code_index` gains additive, read-only secondary roots so dependency/SDK
  symbols are discoverable without clobbering the project index.**
  `hostlib_code_index_rebuild({root})` still owns exactly one writable root and
  flips its slot wholesale, so indexing a dependency root through it would
  destroy the project index. The new `hostlib_code_index_add_readonly_roots({roots, replace?})`
  builds each extra root into a parallel, read-only `IndexState` that lives
  beside the primary. `hostlib_code_index_query` now merges hits from every
  read-only root (each tagged with a `root` field; primary hits carry
  `root: null`), and `hostlib_code_index_read_range` falls back to the
  read-only roots so a symbol discovered in a dependency root can be read back.
  No mutating builtin (`version_record`, `reindex_file`, `rename_symbol`,
  locks) ever touches the read-only set — writes to a dependency-root path stay
  rejected exactly as before. Adding the same root twice is idempotent.
  Enables the deferred burin dependency-grounding wiring (burin #2403 follow-up).
