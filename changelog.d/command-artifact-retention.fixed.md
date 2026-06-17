- **Hostlib command artifacts now expire stale temp directories.** Command output
  artifact creation now performs a throttled best-effort sweep of old
  `harn-command-*` temp directories while preserving fresh artifacts, live-PID
  artifacts, malformed names, and symlinks.
