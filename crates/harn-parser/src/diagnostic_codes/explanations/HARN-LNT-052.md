# HARN-LNT-052 — ambient clock builtin replaced by `harness.clock.*`

**Category:** Lint (LNT)  
**Variant:** `Code::LintAmbientClockBuiltin` (ambient clock builtin)

## What it means

The lint fires on any call to `now_ms`, `monotonic_ms`, `sleep_ms`,
`timestamp`, or `elapsed`. These were ambient clock-capability builtins in
the pre-`Harness` runtime. Time access now routes through the
`harness.clock.*` sub-handle so capability requirements appear in the type
system instead of being hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile while
the migration is in flight, but every new call site should use the
`harness.clock.*` method that matches it (`now_ms` → `harness.clock.now_ms`,
`sleep_ms` → `harness.clock.sleep_ms`, etc.).

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness-clock` repair rewrites every call site where a
  `harness` (or `_harness`) binding is in scope.
- If `harness` isn't reachable from the call site, first thread it through
  the enclosing fn via the `bindings/thread-harness` repair (which adds the
  `harness: Harness` parameter at the entrypoint), then re-run
  `harn fix --apply` to swap the call.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
