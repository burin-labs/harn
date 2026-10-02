- Calls to OpenRouter's Gemini 3.5, 3.6, 3.7, and 3.8 routes no longer fail with HTTP 400 "Reasoning
  is mandatory" when the caller leaves thinking unset. Harn sent a reasoning-disable those endpoints
  reject; it now omits it.
- The OpenRouter QC default and the OpenRouter rungs of the sitrep, judge, and approval-reviewer
  ladders move from `google/gemini-2.5-flash`, which OpenRouter retires on 2026-10-20, to
  `google/gemini-3.5-flash-lite`.
- The catalog adds Kimi K3 on OpenRouter, Qwen3.8 Max on OpenRouter and DeepInfra, and MiniMax M3
  on DeepInfra, each with a capability rule from live probes. Qwen3.8 Max cannot turn reasoning
  off, so it no longer inherits the reasoning-off agent override of the Qwen catch-all.
- DeepInfra Qwen3.7 Max and Qwen3.8 Max validate structured output in Harn instead of sending a
  JSON schema, which the route rejects with HTTP 500.
- OpenRouter Qwen3.6 Max Preview (retiring 2026-10-09), Vercel AI Gateway Gemini 3.1 Flash-Lite
  (2027-05-07), DeepInfra MiniMax M2.7 Turbo, and SambaNova MiniMax M2.7 are marked deprecated with
  their successors. OpenRouter GPT-6.1 Sol rows gain the 272K-token long-prompt price band.
