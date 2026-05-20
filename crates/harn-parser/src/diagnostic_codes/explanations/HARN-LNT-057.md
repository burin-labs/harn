# HARN-LNT-057 — ambient net builtin replaced by `harness.net.*`

**Category:** Lint (LNT)  
**Variant:** `Code::LintAmbientNetBuiltin` (ambient net builtin)

## What it means

The lint fires on calls to the ambient `http_get`, `http_post`,
`http_put`, `http_patch`, `http_delete`, `http_request`, and
`http_download` builtins. Outbound HTTP now routes through the
`harness.net.*` sub-handle so capability requirements appear in the
type system instead of being hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile
while the migration is in flight, but every new call site should use
the matching `harness.net.*` method (`http_get` → `harness.net.get`,
`http_post` → `harness.net.post`, etc.). Streaming, server-mode, and
session builtins keep their ambient names today and will migrate in a
follow-up ticket.

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness-net` repair rewrites every call site where a
  `harness` (or `_harness`) binding is in scope.
- If `harness` isn't reachable from the call site, first thread it
  through the enclosing fn via the `bindings/thread-harness` repair
  (which adds the `harness: Harness` parameter at the entrypoint), then
  re-run `harn fix --apply` to swap the call.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly.
