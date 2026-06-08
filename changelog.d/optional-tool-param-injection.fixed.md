- **Tool middleware optional parameter injection.** `tool_inject_param(...,
  {required: false})` now also marks the injected parameter fragment optional,
  preventing provider-facing tool schemas from accidentally requiring stripped
  middleware-only fields such as `_nl_intent`.
