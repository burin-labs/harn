- Added a default-OFF `code_mode` agent tool (the CodeAct pattern): the model
  authors a short Harn script that composes the session's other tools as a typed
  API via `call_tool(name, args)`, keeping intermediate connector data out of the
  model context and returning only the composed result. The script runs in a
  restricted sandbox VM whose only egress routes through the same policy +
  approval + MCP-credential gate as the model's own tool calls, so a code-mode
  script's capability is provably ≤ the model's own and connector credentials
  never enter the script. Enable per session with `code_mode: true` or by listing
  `"code_mode"` in `enabled_tools`.
