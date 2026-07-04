- **`std/agent/governors` and a unified detector surface in `std/agent/stall`.**
  New composable pace/budget GOVERNORS and a one-vocabulary DETECTOR subsystem,
  generalizing the guardrails a host would otherwise hand-roll on top of
  `agent_loop`. `governor_post_turn(policy)` returns a `post_turn_callback` that
  watches a monotone consumption signal (iterations / tokens / cost) against a
  budget ceiling and steers with a shared `proceed` / `warn` / `abort`
  vocabulary; `compose_post_turn([...])` chains several callbacks and
  `with_governance(opts, {governor, detectors})` folds a governor and a
  `DetectorSpec` into an options dict through existing seams only (no new
  `agent_loop` hooks). `DetectorSpec` lowers loop / no-progress / stuck rows onto
  native `stall_diagnostics` and adds a token-runaway overlay
  (`token_runaway_decision` / `token_runaway_post_turn`) that emits the same
  `agent_loop_stall_warning` event. `governors_selftest()` plus a live-firing
  conformance test assert the callback fires on the real `agent_compute_post_turn`
  payload shape, so a payload drift fails CI instead of silently disabling the
  governor.
