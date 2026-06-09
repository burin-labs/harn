- `harn-vm` stdlib `write_file`/text edits: the primary (no-overlay) write
  path is now crash-safe. It previously used `std::fs::write`, which opens the
  destination `O_CREAT|O_TRUNC` and truncates it before any byte is written, so
  a failure or process kill mid-write (ENOSPC/EDQUOT/EIO) left the file empty
  or partial and the original content unrecoverable. Writes now go through a
  sibling temp file that is flushed, fsynced, and atomically renamed over the
  target, leaving the original untouched on any failure.
