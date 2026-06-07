- **A tool call that the provider cut off mid-emit when the model hit its
  output-token cap is now auto-continued with a raised cap instead of burning
  the turn.** When a value model exhausts `max_tokens` partway through a tool
  call, the provider returns a length-truncation stop reason (`length` for
  OpenAI/OpenRouter/Ollama, `max_tokens` for Anthropic) and the partial output
  carries a truncated, unparseable call. The agent loop previously treated that
  as a malformed/missing call and dropped the turn to parse-guidance — a
  silent-corruption class that wastes a turn even on capable models that were
  mid-correct-action. The loop now detects this specific condition
  deterministically (no model cooperation, no abuse surface): a length
  truncation that resolved zero usable tool calls AND shows a partial-call
  signal (a parser truncation diagnostic or a tool-call opener prefix) is
  re-issued with a higher output cap so the model can finish the call. The
  re-issue is bounded (two continuations by default, each clamped to a ceiling)
  and does not consume a loop iteration; once the cap is exhausted the loop
  falls back to the existing parse-guidance path, so it can never loop forever.
  The gate keys on the normalized finish reason, so it generalizes across
  providers, and it fires ONLY on a real length truncation — a clean stop with
  a genuinely malformed call still flows through the parse-tolerance and
  reasoning-leak paths unchanged, with no overlap.
