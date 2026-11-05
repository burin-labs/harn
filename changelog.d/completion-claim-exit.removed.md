- **The `completion_tool` agent option and its `task_complete` completion-claim exit are removed.** The option was off
  by default and no downstream consumer enabled it; a completion is now accepted only through the ordinary finish path
  and the configured completion judge. The `explicit_completion_claim` stop reason and the
  `harn.completion_claim_observation.v1` checkpoint no longer exist, and `std/agent/completion_claim` is gone.

  Migration: drop `completion_tool` from agent options (it is now an unknown-option error) and stop importing
  `std/agent/completion_claim`.
