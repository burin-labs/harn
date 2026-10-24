- Run records and `harn.run_view.v1` / `harn.session_view.v1` usage now carry
  `cache_read_tokens` and `cache_write_tokens`, summed only over calls whose
  provider reported cache accounting and null when no call did, so an
  unsupported or undeclared route no longer reads as a measured zero. Each
  `llm_call` transcript event records `cache_counts_reported`, and calls
  recorded before that flag existed read as unreported. Session-projected run
  records drop the `metadata.cache_tokens` block, which summed unreported zeros,
  and their per-call `llm_call` spans omit cache counts the call did not report.
