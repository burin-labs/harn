- `harn check` now catches known-bad provider options before a run. Literal
  `effort`/`thinking` settings go through the reasoning gate the runtime
  applies, and text-channel tools passed to a direct `llm_call` get the
  runtime's own refusal. Every refusal or tool-format steer names the catalog
  rule that decided it: the table, the `model_match`, and whether it came from
  a user overlay or Harn's built-in sources.
