Add a self-contained `resolved_dispatch` transcript record emitted per
agent-loop LLM call: the final resolved provider, model, wire format
(`anthropic_native` vs `openai_compat`), base URL host, thinking config, tool
format, per-field provenance (including `inherited_from_primary`), and a
normalized outcome that distinguishes `served`,
`empty_completion_transient_recovered`, `empty_completion_terminal`,
`usage_limit`, and `provider_error`.

A new deterministic
`harn provider dispatch-explain <provider> <model> [--thinking] [--tool-format ...] [--json]`
command reports the same wire-format/tool-format/thinking resolution
statically, with no network or LLM call.
