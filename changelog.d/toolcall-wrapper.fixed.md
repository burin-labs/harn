- **Execute `<tool_call>`-wrapped calls in text/bare mode.** Text-format models
  (e.g. OpenRouter `qwen/qwen3-coder`) wrap their bare `name({ ... })` calls in
  `<tool_call>...</tool_call>` tags even when the prompt asks for bare calls. The
  bare parser now strips those wrapper tags up front, so a same-line
  `<tool_call>run({...})</tool_call>` is executed instead of dropped
  (`tool_calls: []`) and a trailing `</tool_call>` no longer leaks into the
  visible assistant text as a `_call>` fragment.
