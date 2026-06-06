- Conformance fixture `.harn/` dirs now ignore every runtime SQLite DB tests drop in (e.g. the rate-limit
  store `llm-rate-limits.sqlite`), not just the event log, so a stray DB no longer trips the release guard.
