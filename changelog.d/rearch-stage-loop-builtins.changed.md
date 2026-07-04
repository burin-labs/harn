- Internal (pure refactor, no behavior change): extracted the workflow stage
  attempt loop's mechanisms into four runtime-only host builtins —
  `__host_stage_select_artifacts`, `__host_stage_execute_once`,
  `__host_stage_record_attempt`, and `__host_llm_usage_snapshot` /
  `__host_llm_usage_delta` — as inversion pre-work for moving the retry loop
  itself into `std/workflow/stage.harn` (design D5 step 1). The Rust loop in
  `stage.rs::execute_stage_attempts` now drives through the same internal
  functions the builtins wrap; replay, records, and all stage outcomes are
  byte-identical.
