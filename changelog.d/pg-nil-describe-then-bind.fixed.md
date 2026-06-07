- **Fixed dynamic `nil` Postgres binds in all contexts via describe-then-bind.**
  A dynamic `nil` has no static Rust type, so the long-stable fallback bound it
  as `None::<String>` (Postgres TEXT). That poisoned sqlx's per-connection,
  SQL-keyed prepared-statement cache (a later non-text value at the same `$n`
  slot failed with `invalid byte sequence for encoding "UTF8": 0x00`) and was
  rejected outright against non-text typed columns/casts (`column is of type
  integer but expression is of type text`). The previously-attempted OID-0
  ("let the server infer") alternative broke mixed nil + non-null queries with
  `incorrect binary data format in bind parameter N`. Now, **only when a query
  actually contains a `nil`**, Harn prepares the statement with no caller
  parameter types so Postgres infers each slot's OID, then binds every `nil` as
  a typed NULL carrying that server-described OID while non-null params keep
  their natural binary encodings. The all-non-null fast path (and its warm
  statement cache) is completely unchanged. This fixes `nil` into typed columns,
  cache poisoning across NULL-then-non-null on a pooled connection, and mixed
  nil/non-null params in `INSERT`/`WHERE`/`COALESCE`/`CASE`/multi-row `VALUES`.
