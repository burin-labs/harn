# HARN-LNT-053 — ambient stdio builtin replaced by `harness.stdio.*`

**Category:** Lint (LNT)
**Variant:** `Code::LintAmbientStdioBuiltin` (ambient stdio builtin)

## What it means

The lint fires on calls to `print`, `println`, `eprint`, `eprintln`,
`read_line`, and `prompt_user` when a `harness` binding is already in scope.
These were ambient stdio-capability builtins in the pre-`Harness` runtime.
Stdio access now routes through the `harness.stdio.*` sub-handle so capability
requirements are visible in the type system.

This is a lint, not a hard error. The legacy builtins still compile while
the in-tree corpus migration is in flight, but new code should use
`harness.stdio.print`, `harness.stdio.println`, `harness.stdio.eprint`,
`harness.stdio.eprintln`, `harness.stdio.read_line`, or
`harness.stdio.prompt`.

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness-stdio` repair rewrites every call site where a
  `harness` binding is in scope.
- If `harness` is not reachable from the call site, first thread it through
  the enclosing function via the `bindings/thread-harness` repair, then re-run
  `harn fix --apply` to swap the call.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
