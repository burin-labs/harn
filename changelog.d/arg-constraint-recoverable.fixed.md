- Coach a retry instead of a give-up when a tool call is refused by the argument
  allow-list. A path/command outside an agent's scoped `tool_arg_constraints`
  (e.g. a fan-out child scoped to `test/users.*` that tried to edit the shared
  reference file) is now a SOFT, retryable denial: the model is told its allowed
  pattern(s) and to re-issue with a matching argument — and to read reference
  files with `look` rather than editing them — rather than reading the terminal
  "permission denied / do not retry" body and abandoning the turn. Hard
  capability/side-effect/tool ceilings stay terminal.
