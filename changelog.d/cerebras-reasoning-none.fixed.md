- **Cerebras GPT-OSS accepts `reasoning_effort="none"`.**
  The Cerebras `gpt-oss-*` capability rule advertised `reasoning_effort_supported`
  without `reasoning_none_supported`, so an "off" reasoning level floored at
  `minimal` — which Cerebras rejects with `HTTP 400 reasoning_effort: Input
  should be 'none', 'low', 'medium' or 'high'`. That broke every no-tools /
  summarize turn on `cerebras/gpt-oss-120b` (e.g. the release-harness
  audit-finalize turn). The route now advertises `none` as the true
  reasoning-off value.
