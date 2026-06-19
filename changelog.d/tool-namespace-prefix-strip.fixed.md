- Tool-name normalization now strips a leading `tool.` / `tools.` / `functions.`
  / `function.` namespace prefix that cheap OpenAI-compatible hosts (notably
  `gpt-oss-120b`) prepend to native tool calls — `tool.look` → `look`,
  `functions.search` → `search` — so the call resolves to a real tool instead of
  being denied as an unknown name (which previously sent the model into give-up /
  thrash loops). The strip is guarded against the generic-wrapper names
  (`tool.call` / `tool.exec` / `function.call`), which still unwrap their inner
  `{ name, args }` payload rather than collapsing to `call` / `exec`. The
  unknown-tool feedback also recognizes cross-harness edit aliases
  (`apply_patch`, `str_replace`, `str_replace_editor`, `edit_file`, `create_file`)
  and points the model at the `edit` tool instead of issuing a bare denial. This
  is the tool-name-normalization sibling to the tool-format dialect gate.
