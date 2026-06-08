- **`pg_migrate` now recycles prepared-statement caches after DDL, and a few
  `std/postgres` bind/setting edge cases are tightened.** (1) After a migration
  runs DDL, a pooled connection could reuse a cached query plan whose result
  type the DDL changed and fail with `cached plan must not change result type`
  (SQLSTATE `0A000`). `pg_migrate` now clears the pool's cached statements (and
  the per-slot OID describe cache) once after applying any migration, so the
  next query re-prepares cleanly. (2) An out-of-range integer bound into a
  narrow (`int4`/`int2`) column now surfaces a stable `numeric_out_of_range`
  (SQLSTATE `22003`) diagnostic instead of a raw message; in-range integers bind
  and round-trip correctly. (3) A `nil` value in `pg_transaction(settings)` is
  rejected instead of being silently set as the literal text `"nil"`.
