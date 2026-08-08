# HARN-LNT-053 — ambient stdio builtin replaced by `harness.stdio.*`

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

- Run `harn fix --apply --safety surface-changing` over the file. Calls inside
  an existing Harness boundary are rewritten in place; otherwise the fixer
  threads an explicit Harness parameter through local callers.
- Run lint again. `capability-attenuation` suggests replacing an unnecessarily
  broad helper parameter with `HarnessStdio`.
