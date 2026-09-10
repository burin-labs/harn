# HARN-LNT-077 - record literal copies fields one by one

## What it means

A record literal repeats every field name twice to copy fields from one
value:

```harn,ignore
const ctx = {env: harness.env, fs: harness.fs, tools: harness.tools}
```

`pick` does the same job in one call and keeps each field's type:

```harn,ignore
const ctx = pick(harness, ["env", "fs", "tools"])
```

The rule fires only when the rewrite can't change behavior: the literal has
two or more entries, every entry copies a field of the same name from one
value, and each field is one the checker knows is present. That covers the
root `Harness`, a parameter or binding with a record type, and a struct value.
A dictionary key or an optional field stays as it is, because a missing key
gives `nil` in the literal but is left out by `pick`.

## How to fix

Replace the literal with `pick` and the field names in the same order:

```harn
fn main(harness: Harness) {
  const ctx = pick(harness, ["env", "fs"])
  harness.stdio.println(ctx.fs.exists(ctx.env.get_or("APP_CONFIG", ".")))
}
```

`harn lint --fix` and `harn fix --apply` make this change automatically.
