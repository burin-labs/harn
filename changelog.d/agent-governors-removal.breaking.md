- **`std/agent/governors` is removed.** The opt-in post-turn budget governor, the convergence guard, `with_governance`,
  `agent_governed_preset`, `governors_selftest` and `agent_convergence_guard_receipt_schema` had no caller outside tests.
  The loop's own ceilings (`max_iterations`, `token_budget`, `iteration_budget`) are unchanged and end a run with status
  `budget_exhausted`.

  Migration: replace a governor `budget` with `max_iterations` or `token_budget`. `compose_post_turn` now lives in
  `std/agent/postturn`. Importing `std/agent/governors` is a module-not-found error.
