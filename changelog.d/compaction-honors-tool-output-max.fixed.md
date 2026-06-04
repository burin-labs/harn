- **Compaction honors its `tool_output_max_chars` / `compress_callback` policy.**
  The compaction engine parsed, defaulted (16k), and documented these fields but
  never applied them, so oversized tool-result bodies in the kept context window
  stayed at full length. They are now clamped during compaction — via a custom
  `compress_callback` when set, otherwise the built-in microcompactor — while
  each message's `role` and `tool_call_id` are preserved so tool-call pairing
  stays intact.
