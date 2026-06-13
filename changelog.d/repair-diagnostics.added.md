- Agent loop: evidence-aware repair loop in the stall detector
  (`stall_diagnostics.repair_aware`, default off). Tracks a signature-keyed
  current-failure model instead of a blind repeat counter: the same failure
  signature advancing across repair turns — even across intervening edits that
  do not change the error — trips a strategy-shift nudge grounded in the actual
  diagnostic after `stuck_same_diagnostic_after` turns, while a productive edit
  that changes the error resets the streak (so the `fail, edit, fail, edit,
  fail` edit-between-retest thrash is caught, and legitimate progress is never
  flagged). Forces a post-edit re-verify through the existing
  `verify_completion` entrypoint before continuing or stopping, carries a
  `current_failure` summary on a stuck hand-back, and clears it on a successful
  termination so a clean `done` never reports a stale failure. Surfaced
  (default-off) on the tool-using defaults preset's `repair_diagnostics` block.
