- Added a third tool-calling format, `tool_format = "json"` (fenced-JSON), a
  delimiter-safe peer of `text` (tagged/heredoc) and `native`. Each call is one
  ```` ```tool ```` fenced block wrapping a single
  `{ "name": ..., "args": { ... } }` JSON object (N blocks for N calls); the
  body channel is a JSON string, so backticks, `<<EOF`, `}`, and `</tool>` ride
  inside file content with no escaping and the line-anchored close fence never
  collides with a content ```` ``` ````. This root-cause-fixes the
  native/text `<<EOF` heredoc-leak class (`syntax error: line 0: <<`) by
  deleting the heredoc body channel entirely. New parser
  `crates/harn-vm/src/llm/tools/parse/fenced_json.rs` (selected when
  `tool_format == "json"`), new `agent.tool_contract_json` prompt and json
  paradigm/body-hint, format plumbing across the parity gates and capability
  resolution with a compile-time exhaustive `tool_format_channel` guard, and a
  conformance classifier that recognizes a fenced-JSON emission as
  `parseable_harn_text_tool_call`. (A follow-up change promotes `json` to the
  global default text tool-calling format; see the separate changelog entry.)
