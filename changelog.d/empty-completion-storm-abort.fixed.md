- Break the actionless-200 empty-completion re-dispatch storm. When a
  route returns HTTP 200 with an empty visible+tool channel
  (`completion_tokens=8 ... delivered no content` — e.g. an Anthropic
  escalation target fed a huge cross-provider-bridged context), the
  agent loop used to re-dispatch the full context and continue, 18-43x
  per run. Terminal unproductive completions now feed the always-on
  per-route circuit breaker as a dedicated streak (and no longer reset
  it as a success); after a small cap the breaker opens and the next
  dispatch fails fast with `circuit_open` before re-sending the context,
  so the loop degrades onto the primary/cheap-model result instead of
  hemorrhaging full-context re-sends. Provider-general and independent of
  the `llm.rate_governor` flag.
