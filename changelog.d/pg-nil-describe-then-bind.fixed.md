- **Fixed dynamic `nil` Postgres binds in all contexts via describe-then-bind.**
  A dynamic `nil` has no static Rust type, so the long-stable fallback bound it
  as `None::<String>` (Postgres TEXT). That poisoned sqlx's per-connection,
  SQL-keyed prepared-statement cache (a later non-text value at the same `$n`
  slot failed with `invalid byte sequence for encoding "UTF8": 0x00`) and was
  rejected outright against non-text typed columns/casts (`column is of type
  integer but expression is of type text`). The previously-attempted OID-0
  ("let the server infer") alternative broke mixed nil + non-null queries with
  `incorrect binary data format in bind parameter N`. Now, **only when a query
  actually contains a `nil`**, Harn binds every `nil` as a typed NULL carrying
  the server-described OID for that slot while non-null params keep their natural
  binary encodings. The per-slot OIDs are obtained by describing the SQL once and
  **caching the result keyed by the SQL string** — Postgres infers each slot from
  the query structure, so the OID list is stable per SQL and never needs
  invalidation. After the first nil-bearing execution of a given SQL there are
  **zero** extra round-trips: subsequent nil-queries hit the OID cache (no
  describe) and execute as a **non-persistent** statement, so they never poison
  sqlx's SQL-keyed prepared-statement cache and never need to clear it. The
  all-non-null fast path (and its warm statement cache) is completely unchanged,
  and a representative nil-query's steady-state p99 latency is within ~1.02x of
  the same query bound without a nil. This fixes `nil` into typed columns, cache
  poisoning across NULL-then-non-null on a pooled connection, and mixed
  nil/non-null params in `INSERT`/`WHERE`/`COALESCE`/`CASE`/multi-row `VALUES`,
  without the per-query describe + full-cache-clear of the initial fix.
