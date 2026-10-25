- Run records and `harn.run_view.v1` / `harn.session_view.v1` usage now carry
  `cache_read_tokens` and `cache_write_tokens`, summed only over calls whose
  provider reported cache accounting and null when no call did, so an
  unsupported or undeclared route no longer reads as a measured zero. A
  response on a supported route that omits its cache counters is unreported
  too: every provider parser now records whether a counter was present. Each
  `llm_call` transcript event records `cache_counts_reported`, and calls
  recorded before that flag existed read as unreported. Session-projected run
  records drop the `metadata.cache_tokens` block, which summed unreported zeros,
  and their per-call `llm_call` spans omit cache counts the call did not report.

  Migration: `LlmUsageRecord` and `RunViewUsage` gain two public fields, so a
  struct literal must set them. Use `None` when the record has no cache
  accounting, and read a count as unreported when it is `None`.

  ```rust
  LlmUsageRecord {
      cache_read_tokens: None,
      cache_write_tokens: None,
      ..existing_fields
  }
  ```
