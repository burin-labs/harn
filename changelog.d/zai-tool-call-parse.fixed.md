- Recover provider tool-call dialects that previously looked actionless to the
  agent loop: OpenAI-compatible responses that misplaced complete Harn text-tool
  syntax into native `function.name` or `function.arguments`, and DeepSeek DSML
  `tool_calls` blocks that were already recoverable but still injected
  parse-error feedback every turn.
