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

- Run `harn fix --apply --safety scope-local` over the file. By default the
  fixer rewrites ambient stdio calls to an existing local Harness binding or to
  the VM-level `harness` binding with `bindings/use-enclosing-harness-global`,
  preserving helper signatures.
- If you explicitly want source-level parameter threading, run
  `harn fix --apply --safety surface-changing --harness-threading thread-params`.
  `harn fix --plan --json` reports which signatures would change and whether
  cross-module callers must be updated.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
