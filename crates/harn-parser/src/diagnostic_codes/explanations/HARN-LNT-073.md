# HARN-LNT-073 — capability parameter is not named for its capability

A parameter is typed as a narrow capability handle — `HarnessNet`, `HarnessFs`,
`HarnessStdio`, and so on — but is named something else, most often `harness`.
The type says the function holds one capability; the name says it holds root
authority. Readers and call sites believe the name.

```harn
pub fn ack(harness: HarnessNet, url: string) {
  return harness.http_post(url, {})
}
```

Nothing here is unsound, but `harness.http_post(...)` reads like a root handle
with a surprising method on it. The narrowing that
[HARN-LNT-069](HARN-LNT-069.md) asks for is only legible once the name carries
it too.

## How to fix

Name the parameter after the capability's field on `Harness`, which is the same
name a call site already uses to produce it:

```harn
pub fn ack(net: HarnessNet, url: string) {
  return net.http_post(url, {})
}

fn main(harness: Harness) {
  ack(harness.net, "https://example.invalid/ack")
}
```

Harn arguments are positional, so a parameter rename moves no call site.
`harn fix --apply --safety surface-changing` performs it, rewriting the
parameter and every reference to it inside the function.

This also finishes what [HARN-LNT-069](HARN-LNT-069.md) starts. That repair
narrows the type but reuses the existing parameter name, so its output can
still read `harness: HarnessNet`; this lint then renames it.

## When the lint stays quiet

The rename must be provably safe from the function alone, so the lint reports
nothing when:

- the capability's name is already bound in the function — as another
  parameter, or anywhere in the body — because renaming onto it would capture
  that binding;
- a nested function or closure rebinds either name, because its inner
  references belong to a different binding;
- the parameter is a rest parameter, or the type is root `Harness`, which is
  correctly named `harness`.

A dict key that happens to share the parameter's name is a record field, not a
reference, so `{harness: harness}` becomes `{harness: net}` and the record's
shape is unchanged.
