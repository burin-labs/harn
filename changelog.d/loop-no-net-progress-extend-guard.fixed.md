- **`std/agent`: the adaptive loop-control extension policy is now outcome-aware,
  so a thrash can no longer extend its own iteration budget without bound.**
  `agent_loop_is_progressing` keyed "progress" on `progress.changed`, an
  *activity* signal that fires merely because the model issued tool calls — so a
  run thrashing through the same compile/test failure every turn (lots of
  edits/verifies, no error draining toward a green build) read as "progressing"
  every turn and kept hitting the `extend` rule at the budget boundary (observed
  24→32→…→72 on a run that should have been cut). A new default-OFF guard
  (`stall_diagnostics.no_net_progress_extend_guard`, after
  `no_net_progress_extend_after` turns, default 3) sets `progress.no_net_advance`
  when the verify-bearing failing path is *not advancing* — the same failure
  signature has recurred past threshold (the stall detector's
  `same_diagnostic_streak`) — and `agent_loop_is_progressing` then declines to
  extend. Outcome-aware and conservative: a productive edit changes the error
  signature (streak resets → still progress), a passing test clears the failure
  model (→ still progress), and read-only/explore turns with no failing
  verification never arm it, so genuinely-advancing and exploring runs keep their
  budget. New pub `agent_stall_no_net_progress`. Pairs with Burin's product-side
  no-net-progress ripcord: the loop now neither extends a thrash nor lets it run
  unaided.
