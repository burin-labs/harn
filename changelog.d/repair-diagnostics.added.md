- Agent loop: evidence-aware repair loop in the stall detector
  (`stall_diagnostics.repair_aware`, default off). Tracks a current-failure
  model (diagnostic signature + write-epoch) instead of a blind repeat counter,
  forces a post-edit re-verify through the existing `verify_completion`
  entrypoint before continuing or stopping, nudges a strategy shift grounded in
  the actual diagnostic when the same failure recurs across
  `stuck_same_diagnostic_after` repair turns, and carries a `current_failure`
  summary on a stuck hand-back. Surfaced (default-off) on the tool-using
  defaults preset's `repair_diagnostics` block.
