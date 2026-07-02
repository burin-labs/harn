- **Anthropic streaming no longer dispatches tools with silently-empty
  arguments.** When accumulated streamed tool-argument JSON is malformed or
  truncated, the finalizer now emits the same recoverable `__parse_error`
  carrier the OpenAI paths build (the agent loop asks the model to re-issue the
  call) instead of running the tool with `{}`. A genuinely argument-less tool
  call still maps to `{}`.
- **Gemini request lowering stops dropping sampling params.** `seed`,
  `frequency_penalty`, `presence_penalty`, and `logprobs`/`top_logprobs` now
  map into `generationConfig` (capability-gated, matching the OpenAI-compat
  builder).
- **Provider overload (HTTP 529/503, `overloaded_error`) now feeds the
  per-route circuit breaker and the shared cooldown**, so parallel agents back
  off together instead of hammering an overloaded provider. Overload responses
  without a Retry-After header get a default 5s shared cooldown; 429 and
  generic 500/502 semantics are unchanged.
- **Vertex requests now delegate body shaping to the Gemini builder** (as
  Azure delegates to the OpenAI builder), fixing dropped multimodal parts and
  tool-call history and inheriting the sampling-param fix, while preserving
  Vertex-specific auth, model routing, `responseSchema` naming, prefill
  emulation, and the legacy `response_format: "json"` mirror.
