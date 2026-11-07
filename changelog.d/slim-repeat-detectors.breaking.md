- **Agent loop.** Remove repeat detectors, recurring-diagnostic and futility helpers,
  injected stall advice, forced recovery turns, and completion inferred from repeated
  passing checks or unchanged edits. Model-requested work continues within the existing resource limits (#11150).

  Migration: remove `stall_diagnostics` and imports from `std/agent/stall*`. Put post-edit
  and optional terminal verification controls under `repair_diagnostics`. Pass
  `repair_checkpoint` between loops and `repair_checkpoints` between workflows.
  The terminal result no longer reports detector counters or warnings, and call-budget
  projections contain verification and terminal-reporting allowances.
