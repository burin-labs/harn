- **LLM builtin signatures now have a single source of truth in
  `harn-builtin-meta`, eliminating the parser/runtime drift behind #2588.** The
  rich `Ty::Shape` contracts (`LLM_CALL_OPTIONS`, `LLM_CALL_RESULT`,
  `TRANSCRIPT`, `SESSION_SNAPSHOT`, `SUB_AGENT_RESULT`, …) moved from
  `harn-parser` into the dep-free `harn_builtin_meta::shapes`, and the
  `#[harn_builtin]` sig grammar gained an `@NAME` shape-injection form. With
  shapes expressible from a single annotation, the LLM/agent builtins dropped
  `runtime_only = true` and now publish their full signatures through the macro
  — the macro is the authoritative, sole definition. Roughly thirty redundant
  hand-written static parser entries (the `provider_*`/`llm_*` config family,
  `llm_mock*`, `agent_trace*`, `__cache_*`, `with_rate_limit`, …) were deleted
  outright.

  The handful of LLM builtins the typechecker treats as first-class
  (`llm_call`, `llm_call_safe`, `llm_completion`, `llm_call_structured{,_safe,_result}`,
  `schema_recover`, plus `llm_catalog`/`llm_provider_status` reachable via
  `harness.llm.*`) are referenced by `harn-parser`'s own unit tests, which run
  without a driver-installed registry and cannot depend on `harn-vm` (it
  compiles later). Their `BuiltinSignature`s are now defined **once** as
  `pub const`s in `harn_builtin_meta::signatures` and referenced by *both* the
  parser's static table and the macro (via `sig_expr`) — a single definition
  shared across the layer boundary, with no dependency cycle and no second
  place to drift. A `runtime_only`-shadow guard test prevents the original
  drift class from returning, and `sig_expr` builtins still surface a signature
  to `harn explain`/LSP by rendering the parsed `BuiltinSignature` via `Display`.
