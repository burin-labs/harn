- Made fenced-JSON (`tool_format = "json"`) the GLOBAL DEFAULT text tool-calling
  format, replacing heredoc (`text`). A text-channel model with no
  `preferred_tool_format` pin — and the `auto`/omitted resolution path — now
  resolves to `json` in both the runtime (`llm_config::default_tool_format`) and
  the agent stdlib (`std/agent/options` fallback). NATIVE-channel models are
  unchanged. The flip is STRUCTURAL, not just measured: a JSON string can't
  carry a raw newline, so a content delimiter like `<<EOF` never collides with
  the call wrapper, deleting the heredoc `line 0: <<` leak class — so it
  generalizes to unmeasured models, not only the local-qwen3.6 /
  gemini-2.5-flash / deepseek rows that swept a clean 1.0/1.0/1.0
  compliance/parse-determinism/expressiveness bench. Heredoc (`text`) remains a
  selectable format and a per-model `preferred_tool_format = "text"` override
  (the reverse safety valve) for any model that later regresses below baseline.
  `json` is now also a first-class alias `tool_format` (validated against
  text-channel tool support), the structural validator enforces text-protocol
  well-formedness for `json` identically to `text`, and the local-qwen3.6 ollama
  route drops its `text` pin to inherit json (json's ```tool fence sidesteps the
  reserved `<tool_call>` token that forced the heredoc pin).
