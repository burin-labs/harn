- **`std/agent/run_meter` and `std/agent/cut_landing` are removed.** The typed
  run meter, the cut-predicate grammar, and the latched
  `running -> landing -> stopped` machine had no caller in the agent loop, the
  CLI, or any stdlib module. Iteration, cost, token, and deadline ceilings, and
  the message shown when one ends a run, are unchanged and never used them.
  `std/agent/cut_rules` stays.

  Migration: delete imports of either module.
