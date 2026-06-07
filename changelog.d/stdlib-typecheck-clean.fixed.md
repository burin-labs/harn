- **The entire shipped stdlib now passes `harn check` cleanly.** Closing the
  loop on the precise-typing work, eight more latent type bugs were
  root-caused and fixed: nilable option bags narrowed before reaching
  non-nil `dict` builtins (`waitpoint`), missing/mis-typed fields on the
  `context_artifact`, `TriageEvent`, and agent option-bag shapes corrected to
  match what the builders actually emit, a nilable `provider` defaulted before
  a non-nil use (`agent/options`, `agent/sitrep`), a nilable hook registry
  narrowed (`tool_hooks`), and an always-throwing `__fact_error` typed
  `-> never`. The type checker's `.reverse()` was also fixed to return the
  receiver's own type (list-reversing a `list<T>` yields `list<T>`, not
  `string`).
