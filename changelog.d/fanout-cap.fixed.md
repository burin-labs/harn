- Parser: recover an unclosed `<tool_call>` wrapper around a structurally
  complete bare `name({ ... <<EOF ... EOF })` call (heredoc sentinel-closed,
  `})` present, `stop_reason: stop`). It was discarded with a false "TOOL CALL
  TRUNCATED" diagnostic; a genuinely cut-off body still reports truncation.
- Agent loop: add `intra_turn_failure_fanout_cap` (default OFF). When one model
  response fans out a batch of byte-identical *failing* tool calls, collapse the
  tail after the Kth identical failure into a single synthetic result instead of
  dispatching every call — the intra-turn analog of the no-progress terminator.
