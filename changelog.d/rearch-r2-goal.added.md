- **`std/agent/goal`: a typed long-running goal object.** `goal(spec)` normalizes
  `{objective, success_criteria, constraints, budget}`, where each success
  criterion may carry a host-fact `check` callback that makes it machine-checkable.
  `goal_check(goal, facts?)` evaluates that deterministic floor;
  `goal_judge(goal, opts?)` returns a `done_judge` config (the semantic ceiling)
  that composes with the existing `agent_verify_or_continue` seam; and
  `with_goal(opts, goal)` renders the objective/criteria/constraints into every
  outbound request through the existing per-turn context-profile fragment channel
  (#2631) — no new hook surface. `goal_reloop(goal, opts?)` returns `agent_loop`
  options that drive the bounded "not yet met, re-enter with findings" re-loop
  through `agent_loop`'s own completion loop (a `verify_completion` gate over
  `goal_check` vetoes an unmet goal, threads the unmet criteria into the
  transcript, and re-runs the agent up to `max_attempts`, default 3) rather than
  a hand-written loop, and `goal_pin(goal)` bridges a goal into a self-replacing
  `std/agent/pins` pin.
