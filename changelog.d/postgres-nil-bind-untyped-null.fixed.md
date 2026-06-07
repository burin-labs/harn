- **Postgres `nil` bind parameters no longer pin the TEXT type.** The
  `std/postgres` client previously bound a `nil` argument as `None::<String>`,
  which declared Postgres type OID `25` (TEXT) in the wire `Parse` message.
  Because sqlx caches prepared statements per pooled connection and sends
  params in binary, this caused two production failures: prepared-statement
  type-cache poisoning (a slot first seen as `nil` was cached as TEXT, so a
  later non-null integer was UTF-8-validated against TEXT and failed with
  `invalid byte sequence for encoding "UTF8": 0x00`), and wrong NULL typing
  (binding `nil` into an `integer`/`jsonb` column or cast failed with
  `column is of type integer but expression is of type text`). `nil` now binds
  as a Postgres NULL with type OID `0` (unspecified), so the server infers the
  parameter's type from the query context — the cast, the target column — just
  like a bare SQL `NULL`. Non-null binds are unchanged.
