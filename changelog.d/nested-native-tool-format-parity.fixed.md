`agent_loop` now steers an explicit `tool_format: "native"` pin to the route's
safe channel when the route's `tool_mode_parity` forbids native (e.g.
`native_unreliable` routes such as `openrouter/deepseek-v3.2`), matching the
wire-level capability gate in `extract_llm_options`. Previously the stdlib
resolver honored the pin verbatim while the wire silently steered to text, so
the loop built a native-channel prompt (native instructions + native tool
schemas) that disagreed with a text-channel request carrying no tool surface —
the model then saw no usable tool contract and emitted unparseable prose. This
bit nested judge / sub-agent loops that pin `native` for a native-unreliable
route. An explicit `tool_format_override_reason` still forces the requested
channel deliberately.
