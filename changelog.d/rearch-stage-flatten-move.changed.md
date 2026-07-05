- **Stage policy flattening moved to Harn.** The per-stage collapse of the
  ~15 workflow policy structs (model policy, auto-compaction, tool spec,
  capability + approval policy, skill registry, nested-execution attribution)
  into the `agent_loop` options dict now lives in
  `std/workflow/stage.workflow_flatten_agent_loop_options` instead of Rust
  (design D5: Harn decides *what options the loop gets*). Rust keeps only the
  enforcement leaf: it re-derives the capability ceiling
  (`tool spec ∩ stage capability_policy`) and, when the flattened dict re-enters
  the host, rejects any result whose `policy` *widens* that ceiling — a buggy or
  hostile flattener can narrow a capability / budget / permission ceiling but can
  never widen one (`CapabilityPolicy::assert_within_ceiling`, surfaced as a
  `tool_rejected` error). Flattening output is byte-compatible with the prior
  Rust path, so replay records are unchanged.
