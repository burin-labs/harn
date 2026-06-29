- **Worker lifecycle metadata now exposes millisecond timing fields.** Worker
  summaries and `worker_update` bridge events include decoded `created_at_ms`,
  `started_at_ms`, `finished_at_ms`, and `wall_ms` values instead of forcing
  clients to parse UUIDv7 timestamp IDs themselves.
