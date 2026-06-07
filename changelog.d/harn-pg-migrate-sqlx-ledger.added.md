- **`pg_migrate` gained an SQLx-compatible ledger mode (`ledger: "sqlx"`).**
  The Postgres builtin can now read and write SQLx's own `_sqlx_migrations`
  table byte-for-byte: it keys migrations off the integer version prefix of
  each filename, sorts ascending by that numeric version, applies only
  forward files (`*.up.sql` / `*.sql`, skipping `*.down.sql`), records the
  same `version, description, success, checksum (SHA-384), execution_time`
  rows SQLx does, and takes SQLx's per-database advisory lock
  (`0x3d32ad9e * crc32(current_database())`) so a Harn migration and a
  concurrent `sqlx migrate run` serialize against each other. It is
  idempotent against a SQLx-migrated database (applies zero rows, checksums
  byte-identical), refuses to run on a dirty ledger, errors on checksum
  drift naming the version, and warn-and-skips duplicate versions. The
  default `ledger: "harn"` path (the native `harn_migrations` SHA-256
  ledger) is unchanged. This lets harn-cloud retire its bespoke Rust
  `run_migrations()` in favor of `pg_migrate`.
