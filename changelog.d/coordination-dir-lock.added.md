`std/coordination` now exposes filesystem-backed directory lease helpers so Harn
scripts can serialize cross-process work without shell-specific lock glue. Stale
lease recovery is guarded by a second atomic cleanup directory to avoid
cross-process delete/reacquire races.
