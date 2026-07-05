- **OpenAI-compatible tool-call parsing.** Harn now unwraps native
  `tool_call` wrapper functions whose `arguments` string contains a Harn
  text-tool call, preventing providers from dispatching a bogus literal
  `tool_call` when they nest `<tool_call>look(...)` inside the native
  arguments field.
