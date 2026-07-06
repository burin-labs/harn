**Agent stdlib**: consolidated duplicated prompt-fragment folds
(`__with_prompt_fragment`), judge checkpoint preambles
(`__judge_run_checkpoint`), stall observations/diagnostic trips
(`__agent_stall_observation` / `__agent_stall_emit_diagnostic_trip`), and the
`verify` node builder shared by the workflow pattern graphs
(`__patterns_verify_node`). The prompt-nudge overlay fold is now
`with_overlay(agent_options, rows, mode, options?)` — options-first, matching
`with_goal` / `with_governance`; `overlay_policy` remains as a deprecated alias
with the old argument order. Behavior is unchanged.
