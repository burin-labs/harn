- **Corrected gemma-4 / vision / Opus capability declarations.** The local
  (vLLM/SGLang) `gemma-4*` rule now declares native tools + native structured
  output instead of silently degrading to text tools; the Ollama `bakllava` /
  `llama3.2-vision` / `gemma3` rules resolve to `thinking_block_style = "none"`
  so caption models no longer emit a spurious "## Reasoning" scaffold; both
  Ollama `gemma4` rules add `structured_output = "format_kw"` plus explicit text
  tools so JSON/schema output is no longer blocked; and the two Opus 4.6 rules
  use the canonical `structured_output = "tool_use"` instead of the deprecated
  `json_schema` alias. A new audit test walks every catalogued provider alias so
  future tool-capability omissions trip in CI.
