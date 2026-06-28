- **Provider catalog corrections from the cross-provider footgun pre-mortem (harn#3645 §4).**
  Added two reliable Together serverless sample routes
  (`Qwen/Qwen2.5-7B-Instruct-Turbo`, `meta-llama/Llama-3.3-70B-Instruct-Turbo`)
  to replace representative routes that were unusable as one-click samples
  (`Qwen/Qwen3-Coder-Next-FP8` is dedicated-only; the Together Gemma route is a
  reasoning model with an empty-content footgun). Marked the dead
  `MiniMax-Text-01` route deprecated (HTTP 500 / absent from
  `GET /v1/models`; use `MiniMax-M2`). Added three live-catalog contract tests
  that guard the wire-id conventions the audit relied on: no `wire_model` retains
  its own `<provider>/` route prefix, no DeepInfra wire id regains the
  `deepinfra/` prefix, and `nvidia/minimax-m2.7` still dispatches the NIM wire id
  `minimaxai/minimax-m2.7` (#3645).
