- **Hermetic reads of the LLM provider-config env family.** Every `harn-vm`
  production read of `HARN_DEFAULT_PROVIDER`, `HARN_LLM_PROVIDER`,
  `HARN_LLM_MODEL`, `LOCAL_LLM_MODEL`, and `LOCAL_LLM_BASE_URL` now goes through
  a shared `test_env::env_var_seamed` seam instead of `std::env::var`. Under
  `cfg(not(test))` it is exactly `std::env::var(key).ok()`; under `cfg(test)`
  the process environment is structurally invisible and reads come from a
  per-thread override map, so ambient shell configuration or a CI wrapper's
  provider default can no longer leak into a test that did not ask for it. LLM
  provider/model resolution tests inject values through a `TestEnvGuard` that
  clears the map on creation and drop, replacing the hand-rolled
  save/`set_var`/restore boilerplate that previously had to remember every
  variable it touched. This is the third consumer of the keyed-override seam
  pattern (after the unmerged `HARN_EGRESS_*` and `HARN_HANDLER_SANDBOX` seams),
  so the mechanism now lives in one shared owner they can converge onto.
