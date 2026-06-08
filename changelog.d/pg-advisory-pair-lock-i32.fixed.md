- Fixed `pg_advisory_xact_lock` / `pg_with_advisory_lock` failing for string and
  `{class, instance}` keys: the blocking path bound the two-part key as `int8`,
  asking Postgres for a nonexistent `pg_advisory_xact_lock(int8, int8)` overload
  (`function ... does not exist`). The key halves are now bound as `int4` to hit
  the real `(int4, int4)` overload, matching the already-correct
  `pg_try_advisory_xact_lock` path. String keys (`pg_with_advisory_lock(db,
  "migrations", ...)`) and dict keys were the common lock-by-name path, so this
  affected most advisory-lock callers.
