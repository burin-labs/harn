- **`pg_migrate` advisory lock now actually serializes, and the harn ledger
  verifies checksums.** Two correctness bugs in the `std/postgres` migration
  runner are fixed. (1) The Postgres advisory lock was taken, all migration
  work done, and the unlock run on *different* pooled connections. Because
  `pg_advisory_lock` is session-scoped (tied to one backend), concurrent
  `pg_migrate` callers did not mutually exclude, and the unlock usually ran on
  a connection that never held the lock — a no-op that leaked a session lock on
  a recycled connection. The runner now pins a single connection for
  lock → migrate → unlock (matching `sqlx migrate`), in both `harn` and `sqlx`
  ledger modes. (2) The default `harn` ledger wrote a SHA-256 checksum per
  migration but never read it back, so an edited (already-applied) migration
  file was silently skipped with no drift detection. The runner now re-hashes
  each already-applied file and errors with `checksum mismatch for migration
  <name>` when it differs from the recorded checksum, mirroring the `sqlx`
  mode's SHA-384 check. `pg_advisory_unlock`'s boolean result is now checked
  and a `false` (lock not held) is logged.
