- **Structured judge/router calls no longer silently truncate into a dead-judge
  fall-through, and a truncation is now its own integrity category.** A
  `safe_structured_call` (judge / router / cheap-classifier) that went out with
  a tiny `max_tokens` budget truncated mid-object on a reasoning model — gpt-oss
  on Cerebras emits its structured output *inside* the reasoning channel, so the
  reasoning and the JSON share the same output budget — and the unparseable
  result was classified as a generic `missing_json` miss indistinguishable from
  a model that just returned prose. Two provider-generic fixes: (1)
  `safe_structured_call` now floors a structured call's `max_tokens` to 512 (it
  only RAISES an unset/too-small budget; an explicit larger value such as a
  1200-token rubric judge is untouched), so a small verdict object always has
  room to finish; live-probed against `cerebras gpt-oss-120b`, a historical
  `max_tokens: 180` call (which produced zero JSON, 180/180 tokens spent on
  reasoning) now returns clean bounded JSON at ~207 tokens with `stop_reason=
  stop`. (2) A token-limit truncation gets its own `error_category:
  "length_truncation"` on the result envelope (kept even after a failed repair
  pass) so a caller can detect a DEAD structured call — one that fell through to
  a deterministic grader without ever rendering a verdict — instead of laundering
  it as an ordinary abstention.
