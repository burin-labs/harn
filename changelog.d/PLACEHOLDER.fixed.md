Fixed tool calls being silently discarded when a caller pinned `tool_format:
"json"` on a route the catalog pins to heredoc `text`. Parity was enforced at
channel granularity — `json` and `text` are one class for "can this route carry
tool calls in the assistant's content", but the wrong class for "which syntax
will the parser run", since the two grammars are mutually unparseable. The pin
passed both channel gates untouched, the prompt taught fenced-JSON, the wire
used the pinned heredoc grammar, and every call the model made was dropped with
no result, no parse error and no nudge. An explicit `json` now steers to the
route's pinned grammar. The steer is one-way: an explicit `text` on a
`json`-pinned route is left alone, because heredoc is the escape-free body
channel and that request is the documented safety valve for it.
`tool_format_override_reason` still forces either grammar.
