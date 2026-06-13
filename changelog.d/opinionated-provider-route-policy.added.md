- **Opinionated provider-route policy: gpt-oss on OpenRouter is now pinned to
  clean sub-providers, plus a compile-time footgun-validation gate.** OpenRouter
  routes `openai/gpt-oss-120b` across a ~17-upstream sub-provider lottery, and
  some upstreams mis-serialize the Harmony tool call even with reasoning ON
  (billed-noncommittal: 0 tool_calls), so the route was a runtime footgun even
  after the reasoning fix. Two declarative pieces close it: (1) a new
  `openrouter_provider_order` capability field (the allowlist counterpart to
  `provider_route_denylist`) materializes to the OpenRouter request body's
  `provider: { order: [...], allow_fallbacks: false }`, and the
  `openai/gpt-oss-*` OpenRouter row pins it to `["Cerebras", "Groq"]` — the
  upstreams that served Harmony tool calls cleanly in a live 2026-06-13 probe
  (order-pinned requests gave 0 billed-noncommittal; Together was flaky 1/3);
  (2) a data-driven footgun gate (`harn providers build-capabilities --check` /
  `make check-provider-capabilities`) that FAILS the build on known-footgun
  provider/model/config combos — a `reasoning_required_for_tools` route that
  also forces a tool task to reasoning-off, and a `reasoning_required_for_tools`
  OpenRouter route with no clean-sub-provider pin. The gate reads the
  capability matrix's own invariants (no hard-coded model-name patterns), so a
  new footgun route is caught the moment it forgets a pin. The blessed-vs-
  forbidden policy is documented in the `capabilities.toml` base shard.
