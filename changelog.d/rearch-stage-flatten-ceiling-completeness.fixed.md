- **Stage flatten ceiling re-check now covers every widenable dimension.** The
  Rust re-check that rejects a Harn stage flattener from widening the capability
  ceiling (`CapabilityPolicy::assert_within_ceiling`) previously guarded only 7
  of the 10 `CapabilityPolicy` fields, leaving `process_sandbox` (subprocess
  host-FS read/write roots + presets), `tool_arg_constraints` (per-argument path
  scoping), and `tool_annotations` (which drive constraint resolution and
  per-tool side-effect classification) unchecked — a flattener could widen any of
  them undetected. All three are now enforced with the same
  narrowing-allowed / widening-rejected semantics and a `tool_rejected` error
  naming the dimension. The side-effect-level comparison also now ranks through
  the canonical `SideEffectLevel::rank_str` ladder (fail-closed on unknown
  levels, and grows at the top) instead of a hand-rolled fail-open rank.
