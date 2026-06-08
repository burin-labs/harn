- **Fixed nil-bearing Postgres queries whose params Postgres cannot infer
  failing on the new describe-then-bind path.** The describe-then-bind fix learns
  per-slot OIDs via `prepare_with(sql, &[])`, which forces Postgres to infer
  *every* `$n` from query structure alone. For a query with a genuinely
  ambiguous slot (e.g. `SELECT $1 IS NULL`, real failure
  `could not determine data type of parameter $3`), the describe probe itself
  errored and the whole query failed — even though the pre-describe-then-bind
  behavior (bind the `nil` as a `text` NULL) worked. Inside a `pg_transaction`
  the failed probe also aborted the transaction (`current transaction is
  aborted`), so even a same-connection fallback could not recover. The describe
  probe is now **best-effort**: when `prepare_with` fails, Harn no longer
  propagates the error — it returns no per-slot OIDs (cached, since the failure
  is deterministic per SQL structure) and the bind path falls back to the legacy
  `text` NULL, which is strictly ≥ the old behavior. When the probe runs inside a
  caller transaction it is wrapped in a `SAVEPOINT _harn_describe_probe`
  (released on success, rolled back on failure) so a failed probe never taints
  the caller's transaction — the ambiguous query succeeds via the fallback and
  the transaction stays usable for subsequent statements and commit. The
  all-non-null fast path and the successful-describe path are unchanged (no extra
  round-trips when describe succeeds; the savepoint is added only around the
  in-transaction probe).
