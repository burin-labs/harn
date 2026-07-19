- **Durable atomic file writes everywhere.** Four divergent private
  `atomic_write` copies (filesystem overlay commits, filesystem snapshot
  manifests, the credentials file, and the bytecode cache) now share the one
  correct implementation in `harn-vm`'s `atomic_io`, which fsyncs the temp file,
  renames it over the destination, and fsyncs the parent directory. The overlay
  and snapshot copies previously fell back to unlinking the destination before
  retrying the rename, so a crash mid-retry could lose both the old and the new
  file; none of the four synced the parent directory, so a completed write could
  vanish on power loss. The credentials file also gains its `0600` mode on the
  temp file before the rename rather than on the destination afterwards, closing
  a window in which stored secrets were readable at the process umask default.
