- **Anthropic provider requests.** Runtime feedback injected between an
  assistant `tool_use` and its matching `tool_result` is now deferred until
  after the result before sending Anthropic Messages API requests, avoiding
  non-retryable 400 responses on strict tool-result adjacency validation.
