# HARN-LNT-071 — global builtin replaced by a Harness method

This builtin was called as a global, but it performs an effect, so it now lives
on a `Harness` capability instead. Reaching it through the harness is what makes
a script's effects readable from its signatures.

## How to fix

Call the method on the handle the diagnostic names, and pass that handle into
the helper that needs it:

```harn
fn report(stdio: HarnessStdio, message: string) {
  stdio.println(message)
}
```

Some globals that returned a single value are now a field on a structured
snapshot. For example, `platform()` becomes `harness.system.platform().os`, and
`username()` becomes `harness.system.identity().username`.

Keep the root `Harness` at entrypoints and at boundaries that genuinely
coordinate several capabilities. Elsewhere, pass the narrowest handle that
covers what the function does.

`harn fix --apply --safety surface-changing` rewrites these calls for you,
including calls inside `${...}` string interpolation, and adds the parameter to
the local callers that need to supply the handle.
