- Provider-side prompt caching now defaults ON for routes whose capability
  matrix declares `prompt_caching`. The stable system-prompt + tool-definitions
  prefix re-sent on every turn of a multi-turn agent loop (and across the rubric
  grader's turns×trials) is marked cacheable, so supporting providers discount
  it heavily — Anthropic ephemeral caching (~90% off cached input), OpenRouter
  `cache_control` passthrough, and implicit DeepSeek / gpt-oss caching. The win
  is largest for the cheap value models the product steers toward. Routes that
  do not advertise prompt caching are unaffected: the resolved `cache` flag
  defaults to `false` for them, leaving the outgoing request byte-identical. An
  explicit `cache:` option is always honoured verbatim — `cache: false` opts out
  anywhere, and an explicit `cache: true` on a non-caching route still errors
  loudly via the capability gate.
