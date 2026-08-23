- Reasoning-effort capability is now one claim instead of three that could
  disagree. A capability fragment can spell "this route takes an effort ladder"
  three ways — `thinking_modes` containing `effort`, the legacy
  `reasoning_effort_supported` flag, and a non-empty `reasoning_effort_levels` —
  and the last two are now absorbed into `thinking_modes` rather than compared,
  so a contradiction is unrepresentable rather than merely detectable. Nineteen
  shipped routes were contradicting themselves, and both directions were broken
  at runtime: the reasoning policy read one field and built an effort request,
  and the option validator read another and refused the request the policy had
  just built. `harn` rejected an explicit `effort` on `gemini:gemini-2.5-pro`,
  whose own row lists the `effort` mode, and refused every effort request to
  `openrouter:z-ai/glm-5*`. Both failed with the same message a route that
  genuinely has no effort control produces, which is why neither was visible.
- `openrouter:z-ai/glm-5.3` no longer fails every agent, verify, and code task.
  The route always reasons and answers HTTP 400 to a disable directive
  ("Reasoning is mandatory for this endpoint"), but its capability row declared
  the `enabled` thinking mode alongside `auto_reasoning_overrides = { agent =
  "off", verify = "off", code = "off" }`, so the default auto reasoning policy
  resolved those tasks to a disable directive and every such call took the 400.
  Its effort ladder was also asserted rather than measured: the row declared
  `["high", "xhigh", "max"]` while the route serves `minimal` through `max`.
- `gemini-3.6-flash` can disable thinking. The 3.6/3.7 family row claimed
  `reasoning_disable_supported = false` because that is the Pro-class
  contract; a live effort-probe sending `thinkingBudget: 0` was accepted and
  produced 1 output token against 62/87/83 at low/medium/high. The Flash
  row now says so, so a caller asking for `effort: none` actually reaches
  the wire instead of being silently dropped.
- `llm_call` failures now carry `origin`, either `"provider"` or `"local"`.
  Harn's own pre-dispatch option checks and a provider's HTTP 400 were both
  `terminal`/`invalid_request` with nothing to separate them, so a caller
  deciding whether to retry, fall back, or correct its own request could not
  tell whether the request had ever left the process.
