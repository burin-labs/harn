- `std/postgres/query` projection helpers (`uuid_text`, `timestamptz_json`,
  `nullable_timestamptz_json`, `select_clause`) now return trusted
  `PgSqlFragment`s, and a new `columns(parts)` helper joins projection
  fragments/strings into one `{projection}` fragment. Column projections now
  drop into `sql(...)` placeholders without an `unsafe_sql(...)` wrapper, and
  carry the literal `'{}'` JSON path safely.
