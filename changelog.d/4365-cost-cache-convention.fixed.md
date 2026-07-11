- Stop undercounting Anthropic session cost: `project_call_cost` now detects
  whether `input_tokens` includes cached tokens (OpenAI) or excludes them
  (Anthropic) before subtracting cache counts, so real non-cached input is no
  longer billed at zero.
