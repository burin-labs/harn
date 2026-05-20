# HARN-LNT-053 — ambient stdio builtin replaced by `harness.stdio.*`

**Category:** Lint (LNT)
**Variant:** `Code::LintAmbientStdioBuiltin` (ambient stdio builtin)

## What it means

The lint fires on calls to `print`, `println`, `eprint`, `eprintln`,
`read_line`, and `prompt_user`. These were ambient stdio-capability builtins
in the pre-`Harness` runtime. Stdio access now routes through the
`harness.stdio.*` sub-handle so capability requirements are visible in the
type system.

This lint is emitted during auto-repair planning so existing call sites can be
migrated before the removed builtin produces an unknown-name diagnostic. New
code should use `harness.stdio.print`, `harness.stdio.println`,
`harness.stdio.eprint`, `harness.stdio.eprintln`,
`harness.stdio.read_line`, or `harness.stdio.prompt`.

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness` repair rewrites every call site where a
  `harness` binding is already in scope, and can thread that existing binding
  down same-file synchronous helper chains.
- If `harness` is not reachable from the call site, `harn fix --plan` will
  surface the `bindings/thread-harness-needs-param` repair instead. That path
  adds a `harness: Harness` parameter and updates local callers, so it is
  marked `surface-changing`.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
