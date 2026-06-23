- Agent loop: `intra_turn_failure_fanout_cap` now collapses EVERY distinct
  identical-failure group in one response, not just the first. A single
  `collapsed_emitted` latch let the first capped fan-out group suppress the
  collapse marker for every later group in the same batch, silently dropping
  those groups' tail calls from the result set with no entry at all. The latch
  now resets whenever a new call signature trips the cap, so each group emits
  exactly one synthetic collapsed result (regression-guarded by a two-group
  scenario in `agent_loop_intra_turn_failure_fanout_cap`).
