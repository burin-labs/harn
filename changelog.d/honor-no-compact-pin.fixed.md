- **Transcript compaction now honors the host `[no-compact]` pin so an agent's
  live grounding survives a compaction pass.** Both compaction surfaces in the
  `observation_mask` strategy — archived-window masking and kept-window
  `clamp_tool_outputs` length-clamping — now treat any tool-output or message
  body that contains the literal `[no-compact]` marker (emitted by the host
  around the current file view and the just-edited window) as pinned: masking
  preserves it verbatim and clamping leaves it intact. Previously the marker was
  ignored, so on long sessions the model lost sight of the file it was editing,
  then drifted, re-read, and stalled. The pin is bounded to the most recent
  `MAX_PINNED_SEGMENTS` (3) pinned bodies — older duplicate snapshots from
  earlier in the session compact normally — so a pin can never accumulate
  unbounded and overflow the context window. With no pins present, compaction
  behaves exactly as before.
