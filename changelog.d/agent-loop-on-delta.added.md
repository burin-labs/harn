- **`agent_loop` gains an `on_delta` streaming seam (#4020).** Pass an
  `on_delta: { delta -> ... }` closure and each per-turn model call is issued
  through the streaming transport, firing the callback once per streamed chunk of
  the assistant's visible text — so chat-shaped harnesses can render or transform
  the token stream without abandoning `agent_loop` for a raw `llm_stream_call`.
  The callback is observational (return value ignored); the turn still returns a
  complete result, so native tool calls and usage are preserved and tool dispatch
  is unaffected. Providers that do not stream fall back to a single delta carrying
  the full visible text. A custom `llm_caller` short-circuits the default path, so
  a non-streaming caller simply never fires `on_delta`. Tool-call-fragment
  streaming is intentionally out of scope for v1. The `llm_mock` testing surface
  learns a `stream_chunks` field (helper: `llm_stream_text([...])`) for scripting
  deterministic streaming responses.
