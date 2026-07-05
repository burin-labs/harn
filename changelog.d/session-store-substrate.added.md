- Added the `std/session-store` primitive: an append-only, SHA-256 hash-chained session event store at
  `.harn/session-store/<session_id>.jsonl`. `session_store_append` writes events (mirroring the harn-serve
  `StoredEvent` shape) with a canonical `record_hash` over `{session_id, event_id, payload, prev_hash}`;
  `session_store_project` / `session_store_project_value` fold `upsert`/`delete`/`replace`/`clear` mutation
  payloads to the latest-by-id records or a single value; and `session_store_verify` proves chain integrity.
  This is the durable substrate agent memory, hypothesis, and learned-context stores layer on.
