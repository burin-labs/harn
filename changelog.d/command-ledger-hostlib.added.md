- Background `run_command` handles gained the telemetry a controlling agent loop needs to schedule its own
  decision cadence: running snapshots now carry a monotonic `output_offset` (so a consumer pages only the delta
  since its last read), a `stderr_byte_count`, and `silence_ms` since the last output chunk; each handle records an
  `awaited`/`service` lease tag; and a new `tools.list_handles` builtin enumerates the live handles for a session
  (empty once each completes and its waiter drains it).
