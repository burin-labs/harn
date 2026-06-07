- **`tool_format` is now reject-or-work-well: a bad value fails loudly instead
  of silently degrading.** The agent-loop `tool_format` knob
  (`agent_tool_format_resolution` in `std/agent/options`) previously accepted
  any explicit string verbatim — a typo like `"nativ"` or a wrong value like
  `"json"` / `"tool_use"` flowed straight through as `source: "explicit"` with
  no warning, and every downstream branch that gates on `tool_format ==
  "native"` read it as `false`, so the agent silently ran the text protocol.
  Resolution now throws on any value that is not `native`, `text`, `auto`, or
  omitted. It also rejects requesting the side the capability matrix marks
  *impossible* for a model (`native` on a `text_only` model, or `text` on a
  `native_only` model); pass `tool_format_override_reason` to force the marked
  side deliberately (probe/matrix use). `*_unreliable` parity stays a
  recoverable warning, not a hard reject.
- **The provider-catalog validator now rejects alias `tool_format` pins that
  the target model cannot serve.** An alias may only pin `tool_format =
  "native"` / `"text"`, and only when the model's `tool_support` advertises
  that side. This caught a real shipped footgun: the
  `ollama-devstral-small-2-native` alias pinned `native` on a model the
  capability matrix marks `native_tools = false` (`text_only`). That alias has
  been removed — Devstral Small 2 on Ollama is text-tool-only.
- Added a deterministic tool-calling boot-camp battery
  (`crates/harn-vm/tests/tool_calling_bootcamp.rs`) that exercises the real
  resolution layer across a pairwise sample of {capability-profile ×
  requested-format × config-source} and asserts the reject-or-work-well
  invariant with zero live LLM calls.
