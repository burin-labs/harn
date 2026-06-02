- **File event log `flush()` no longer fails on Windows.** `sync_tree` opened each topic/consumer
  file read-only and called `sync_all()`, which on Windows lowers to `FlushFileBuffers` — and that
  requires a write-access handle, so it failed with "Access is denied" (on Unix, `fsync` on a
  read-only descriptor is fine, which masked the bug). `flush()` now fsyncs through a hardened
  `fsync_file` helper that opens for write, with a read-only fallback so a durability flush can never
  hard-error on a genuinely read-only file. Fixes the `session_timeline::persisted_file_log_reads_agent_events`
  failure that was red on the Windows CI lane (and blocking docs auto-deploys).
