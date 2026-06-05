- **Postgres query templates.** `std/postgres/query` now includes
  `sql(...)` and `named_sql(...)` helpers that turn readable `{name}` SQL
  templates into `$n` parameterized query records, plus explicit identifier and
  source-controlled fragment helpers for SQL structure.
