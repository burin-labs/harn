- **`std/agent/stall` gains a host verify-state progress axis.** A new optional
  `stall_diagnostics.progress_signal` callback (`{payload -> float?}`) lets the
  host report a monotone "best verification state" scalar so
  `agent_stall_no_net_progress` keys on whether the verifier actually advanced
  ("writes are not progress") instead of edit/tool signatures. It expresses both
  a long-stall cut (`verify_state_streak` + `verify_state_stall_turns`, or a
  `verify_state_recurrence_hard` recurrence) and a short write-axis cut
  (`no_verifier_progress_limit` non-advancing turns with a write landed). Absent
  the callback, behavior is byte-identical to before.
- **`std/agent/stall` gains a delivered-fix-not-landing trigger.** A new optional
  `stall_diagnostics.remediation_delivered` callback
  (`{ {session_id, signature, prev_dispatch} -> bool }`) lets the host report
  that a repair was delivered for the active failure signature; when it still
  recurs, the detector escalates one turn sooner via the new
  `delivered_fix_not_landing` warning pattern. Absent the callback, the plain
  `stuck_same_diagnostic` nudge is unchanged.
- **`std/agent/governors` gains `governor_pace_decision(policy, obs)`.** A pure
  smart-timeout pace core (proceed / extend / pace_check / cut) that decides
  progress-based *extend-inside-a-time-bound*, returning `new_budget_ms` for
  host-side wall-budget re-stamping. Bounded by `extend_max` /`pace_check_max`
  (default the existing `GOVERNOR_PACE_EXTEND_MAX` / `GOVERNOR_PACE_CHECK_MAX_INJECTIONS`).
  It reads no clock, store, or flags — every input is passed in.
