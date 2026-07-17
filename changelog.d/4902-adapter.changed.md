Route `std/session-store` through the canonical shared SQLite store while
preserving parent-event links and the per-stream `session_store_path` contract;
the explicit `session_store_database_path` exposes the shared database. Retired
JSONL streams import atomically with strict chain, tenant, and metadata
validation, including redaction inside preserved source-header envelopes and
flattened projections. The retired `options.now` override now fails loudly.
