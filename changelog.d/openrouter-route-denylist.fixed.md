- **OpenRouter route-around for broken upstreams plus a billed-no-op contract
  guard.** Capability rows gained a data-driven `provider_route_denylist`
  (`Vec<String>`) that the OpenAI-compatible request builder materializes into
  the OpenRouter request body's `provider.ignore` array (merged and deduped,
  preserving existing entries). The `qwen/qwen3.6*` openrouter row denies the
  `Ambient` upstream, which billed reasoning tokens and then finished with
  `finish_reason: "stop"` and empty `tool_calls` — narrating the intended tool
  call on the reasoning channel and serializing it to no wire field — while
  `Parasail` / `AtlasCloud` / `AkashML` serve the identical request natively.
  Native tools stay on; only the broken upstream is routed around (the
  `require_parameters` knob does not help). As a deterministic backstop across
  every OpenAI-compatible route (streaming and non-streaming), a clean-finish
  turn that billed output, offered tools, captured no tool call or tool-search
  block, and produced fewer than 24 committed answer characters now fails loudly
  as an "upstream contract violation" instead of returning a silent empty
  success. A `length`/truncation finish, a normal tool call, a substantive text
  answer, and a tool-less prompt are all left untouched.
