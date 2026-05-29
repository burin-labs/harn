- **LLM config builtin signatures now agree between the runtime descriptor
  and the parser registry (found while fixing #2588).** The `#[harn_builtin]`
  cutover (#2575) left several `runtime_only = true` LLM builtins with a
  hand-written static parser signature that had drifted from the authored
  `sig`. `provider_capabilities`' `model` parameter is now `string|nil`
  (matching the runtime, which accepts a nil model) instead of the narrower
  `string`, and the coarse runtime `sig` strings that `harn explain` / LSP
  surface are corrected to match actual return values:
  `provider_capabilities_clear`, `provider_capabilities_install`, and
  `provider_register` return `bool`, `llm_config` returns `dict|nil`, and
  `llm_rate_limit` returns `bool|int|nil`. No runtime behavior changes — only
  the advertised types. `runtime_only` is retained because the parser entries
  for the richer LLM builtins (`llm_call`, transcript helpers, …)
  intentionally carry typed shapes the `sig` grammar cannot express.
