- **The OpenAI-compatible transport now honors the model catalog's
  `stream_timeout`, surfaces mid-stream failures, and retries zero-token empty
  completions.** Three gaps turned provider stalls into silent empty agent
  turns (observed live in Burin Code eval-meter work: an OpenRouter call hung
  133s and returned `output_tokens=0` as a "success"): (1) the catalog's
  `stream_timeout` (seconds) was projected into config dicts but consumed by
  no transport — it now feeds the shared whole-request deadline
  (`explicit timeout option > HARN_LLM_TIMEOUT > stream_timeout > 120s
  default`) for every provider on the common `resolve_timeout` seam, both
  streaming and non-streaming, so slow local models with `stream_timeout =
  900.0` get their budget and hung remote calls are bounded; (2) a mid-body
  SSE read failure (including that deadline firing mid-stream) was silently
  swallowed, returning a truncated zero-token success — it now surfaces as
  the same transient stream-error class other timeouts use, so the existing
  retry machinery picks it up; (3) a wire-level "success" carrying zero output
  tokens, no content, no thinking, and no tool calls is now retried once
  built-in (more with `llm_retries`) as a transient provider hiccup, with an
  `empty_completion_retry` observability entry and `EmptyCompletionRetry`
  trace event; if it stays empty after the budget, the result is returned
  unchanged. Token-cap truncations (`stop_reason` length/max_tokens) are
  excluded from the retry, and mock/fake providers only retry on explicit
  opt-in so scripted tests stay deterministic.
