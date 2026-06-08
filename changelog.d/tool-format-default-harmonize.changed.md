- Harmonized `gpt-oss` to a single tool-format default and dropped the stale
  heredoc `text` pins left on the local devstral / llamacpp-qwen rows after the
  fenced-json default flip. Previously the same `gpt-oss` model resolved three
  ways: cerebras and groq pinned `preferred_tool_format = "native"` while
  together inherited the new global `json` default — a correctness bug. All
  three `gpt-oss` capability rows now inherit `json` (the cerebras/groq rows drop
  their `native_tools`/`preferred_tool_format = "native"` pins so they match
  together). native is the evidenced-bad direction here: `gpt-oss` native
  streaming tool-calls have returned empty payloads in evals; `json` is
  structurally delimiter-safe and beats heredoc `text`.
- Dropped the explicit `preferred_tool_format = "text"` pins on the llamacpp
  `*qwen3.6*` / `*qwen3*` and the llamacpp + ollama `devstral-small-2*`
  capability rows so they inherit the global `json` default like their siblings.
  The qwen rows keep `reserved_tool_call_token = true` (the remap still applies
  whenever a heredoc `text` pin re-selects the tagged format; json's ` ```tool `
  fence already sidesteps the reserved `<tool_call>` token). devstral has no
  reserved-token constraint, so there was never a structural reason for the
  heredoc pin. Confirmed live on the local-qwen3.6 `:8001` route (json parses
  delimiter-soup `write_file` content clean; heredoc leaks the `<<EOF`
  delimiter); the devstral rows apply the same structural fix (not locally
  reachable, backed by the audit's local-qwen3.6 json 3/3 vs heredoc 0/3).
