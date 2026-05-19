# HARN-LNT-055 — ambient env builtin replaced by `harness.env.*`

**Category:** Lint (LNT)  
**Variant:** `Code::LintAmbientEnvBuiltin` (ambient env builtin)

## What it means

The lint fires on calls to the ambient `env` and `env_or` builtins.
Environment access now routes through the `harness.env.*` sub-handle so
capability requirements appear in the type system instead of being
hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile
while the migration is in flight, but every new call site should use
`harness.env.get(name)` / `harness.env.get_or(name, default)`.

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness-env` repair rewrites every call site where a
  `harness` (or `_harness`) binding is in scope.
- If `harness` isn't reachable from the call site, first thread it
  through the enclosing fn via the `bindings/thread-harness` repair
  (which adds the `harness: Harness` parameter at the entrypoint), then
  re-run `harn fix --apply` to swap the call.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly.
