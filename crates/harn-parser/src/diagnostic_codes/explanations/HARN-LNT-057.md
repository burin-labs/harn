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

- Run `harn fix --apply --safety scope-local` over the file. By default the
  fixer rewrites ambient network calls to the VM-level `harness` binding with
  `bindings/use-enclosing-harness-global`, preserving helper signatures.
- If you explicitly want source-level parameter threading, run
  `harn fix --apply --safety surface-changing --harness-threading thread-params`.
  `harn fix --plan --json` reports which signatures would change and whether
  cross-module callers must be updated.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly.
