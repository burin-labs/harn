- **Builtin signature shapes are now shared, type-safe vocabulary in
  `harn-builtin-meta`, and LLM builtins publish their signatures through the
  `#[harn_builtin]` macro instead of hiding behind `runtime_only` (follow-up to
  #2588).** The structural-record `Ty::Shape` consts (`LLM_CALL_OPTIONS`,
  `LLM_CALL_RESULT`, `TRANSCRIPT`, `SESSION_SNAPSHOT`, `SUB_AGENT_RESULT`, …)
  moved from `harn-parser` into the dep-free `harn-builtin-meta::shapes`, so the
  typechecker and the runtime descriptor reference one definition. The
  `#[harn_builtin]` sig grammar gained a `@NAME` injection form
  (`options?: @LLM_CALL_OPTIONS) -> @LLM_CALL_RESULT`) that resolves to those
  shared consts, letting the LLM/agent builtins (`llm_call`, `llm_call_safe`,
  `llm_completion`, `llm_stream`, `llm_call_structured*`, `schema_recover`, the
  `provider_*`/`llm_*` config family, …) drop `runtime_only = true` and publish
  their full, rich signatures — the macro is now the authoritative source when
  the registry is installed. A new `runtime_only_builtins_never_shadow_a_static_parser_entry`
  guard test makes the original drift class impossible: a `runtime_only` macro
  builtin may never also carry a hand-written static parser entry. The static
  tables are retained only as `harn-parser`'s self-contained fallback for
  standalone typechecking (e.g. its own unit tests), now sharing the same
  `harn-builtin-meta` shapes so they cannot diverge structurally.
