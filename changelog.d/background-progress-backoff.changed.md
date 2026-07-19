- Background `run_command` progress feedback now follows an exponential backoff schedule instead of a fixed
  interval: it starts at `progress_interval_ms`, doubles the delay after each snapshot, and is clamped by the new
  optional `progress_max_interval_ms` request field (default 30000ms, never below the base interval). A
  long-running command emits frequent early progress that thins out over time, so a quiet multi-minute command
  stays cheap while its completion snapshot is still published on exit.
