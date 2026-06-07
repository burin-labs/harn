- Fixed the direct Anthropic provider sending `tool_choice` as a bare string
  (the OpenAI wire shape), which Anthropic rejected with HTTP 400
  (`tool_choice: Input should be an object`) and broke tool-using agent loops on
  `--provider anthropic`. Harn's tool-choice modes are now mapped to Anthropic's
  object form (`auto`/`any`/`none`/specific-tool), and OpenAI-style
  `{"type":"function",...}` and bare-name inputs are normalized too.
