- Fixed the Postgres hostlib binding a non-finite `Float` (NaN/Infinity) raw:
  a direct `float8` bind stored NaN/Infinity (which breaks downstream JSON), and
  a non-finite float on the jsonb path serialized to a silent JSON `null`.
  `pg_query`/`pg_execute` (and the introspection/advisory helpers) now reject a
  non-finite float — directly bound or nested in a list/dict — with a clear
  error before it reaches the database. Finite-float binds are unchanged.
