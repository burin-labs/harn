- Claude Fable 5.1 (`claude-fable-5-1`) is in the Anthropic catalog, reachable
  as `fable` or `fable51`; `fable5` still addresses the previous generation.
  It is Mythos-class, above Opus, so the `opus` alias keeps tracking the Opus
  generation. Cache reads are billed at their own rate rather than the uniform
  ratio every other Anthropic row follows.
- A route that refuses forced tool choice no longer breaks
  `output_format: json_object`. That request used to pin `tool_choice` to a
  synthetic tool, which Claude Fable 5.1 rejects outright; where the route
  speaks native structured output it now takes that path instead. Routes that
  still accept a forced tool choice are unchanged.
