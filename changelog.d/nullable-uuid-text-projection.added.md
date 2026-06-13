- `std/postgres/query` gains `nullable_uuid_text(name)` — the nullable counterpart to `uuid_text(name)`, rendering
  `CASE WHEN name IS NULL THEN NULL ELSE name::text END AS name` as a trusted projection fragment. It preserves SQL
  NULLs as JSON `null` instead of casting them to the string `"null"`, mirroring `nullable_timestamptz_json`, and
  accepts table-qualified names (`sessions.forked_from_session_id`). This replaces the hand-rolled `CASE WHEN … END`
  string concatenation that data-access modules wrote for nullable UUID/foreign-key columns.
