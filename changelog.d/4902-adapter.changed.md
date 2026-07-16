Route `std/session-store` through the canonical shared SQLite store, preserving
the prior parent-event links while moving path results to the shared database.
Retired JSONL streams import atomically with strict chain, tenant, and metadata
validation, including redaction inside preserved source-header envelopes;
`options.now` remains accepted but canonical timestamps now win.
