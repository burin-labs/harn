- Typed option aliases are now the single documented path for building
  orchestration options. `std/llm/options` gains `LlmCallOptions`, `LlmBudget`,
  and `ModelLadder` (plus `llm_options(...)` / `model_ladder(...)`
  constructors); `std/agent/options` gains `AgentLoopOptions`,
  `AgentPresetOptions`, `IterationBudget`, `TurnPolicy`, `StallDiagnostics`,
  `CompactionPolicy`, and `JudgeConfig` (plus `agent_options(...)` /
  `agent_preset_options(...)`); `std/workflow/options` gains `StageSpec`,
  `WorkflowRetryPolicy` (including the staged `repair_prompt_builder` /
  `feedback` retry-with-feedback keys), `ModelPolicySpec`, `StageContract`,
  and `WorkflowExecuteOptions` (plus `workflow_stage_spec(...)` /
  `workflow_execute_options(...)`). Each alias carries a cross-reference to
  its Rust policy twin and a serde-defaults key-parity test pins the two
  surfaces together. A new info-level `unnormalized-options` lint
  (`HARN-LNT-060`) flags inline option dict literals passed directly to
  `agent_loop` / `workflow_execute` and points at the typed constructors —
  raw dicts still execute unchanged. Docs examples across `llm_call`,
  `agent_loop`, the workflow runtime chapter, and the LLM quickref now build
  options through the typed aliases.
