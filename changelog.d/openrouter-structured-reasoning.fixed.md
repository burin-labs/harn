- **OpenRouter structured calls to non-reasoning models no longer 404.** When a
  model declares no reasoning capability, harn no longer emits a
  `reasoning: {enabled: false}` disable directive alongside
  `require_parameters: true` — that combination made OpenRouter drop every
  endpoint that doesn't support the reasoning param (e.g. `qwen/qwen3-coder`
  JSON-schema calls returned `404 No endpoints found`). The disable was a no-op
  for these models anyway.
- **Truncated reasoning no longer leaks into the final answer.** On an
  OpenAI-compatible response cut off at `finish_reason: "length"` with empty
  content, harn no longer promotes the partial reasoning trace into
  `.text`/`.visible_text`; the surfaced answer stays empty/flagged and the
  partial trace is exposed via `thinking` instead.
- **Unknown-model errors classify uniformly.** OpenRouter reports an unknown
  model as an HTTP-400 `"<id> is not a valid model ID"` body (no status/typed
  signal); harn now maps that prose to `NotFound`/`model_unavailable`, matching
  Cerebras's 404 path.
