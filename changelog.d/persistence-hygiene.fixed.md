- **Worker snapshots no longer silently corrupt non-serializable values or
  persist secrets verbatim.** A new strict persistence serializer
  (`vm_value_to_json_strict`) rejects closures, channels, and other
  runtime-only handles at save time with a path-annotated error (e.g.
  `options.custom_compactor: closure is not serializable`) instead of
  writing a display-string that rehydrates as a plain string and fails long
  after resume. Workflow worker options fail loud; live sub-agent suspend
  options (which legitimately carry callbacks like `tool_caller`) strip the
  offending entry with a WARN event naming the dropped path. Every persisted
  worker snapshot is now scrubbed with the unified redaction policy, so
  `Authorization`/api-key-shaped fields and high-confidence token patterns in
  options, headers, and transcripts land on disk as `[redacted]`.
