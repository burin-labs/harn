# Pick fields from a record

`pick(source, keys)` builds a new record from the fields you name. It's a
global builtin, so you don't import it.

```harn
type Context = {env: HarnessEnv, fs: HarnessFs}

fn config_exists(ctx: Context) -> bool {
  const path = ctx.env.get_or("APP_CONFIG", "harn.toml")
  return ctx.fs.exists(path)
}

fn main(harness: Harness) {
  const ctx = pick(harness, ["env", "fs"])
  harness.stdio.println(config_exists(ctx))
}
```

`ctx` has the type `{env: HarnessEnv, fs: HarnessFs}`. Each handle keeps
its grants, so `config_exists` can call methods on it.

## Arguments

| Argument | Accepts |
|---|---|
| `source` | A record, a dictionary, a struct value, or the root `Harness` |
| `keys` | A list of strings. Each string names one top-level field |

## What pick returns at runtime

- Every picked value is kept as-is, including `nil`, `false`, `0`, and `""`.
- A key the source doesn't have is skipped.
- A repeated key produces one field.
- An empty list returns `{}`.
- Keys don't reach into nested records. `"a.b"` looks for a field named `a.b`.
- The source isn't changed. Editing a nested record in the result leaves the
  source alone. Capability handles stay shared with the source.
- Any other kind of source, or a key that isn't a string, is a runtime error.

```harn
fn main(harness: Harness) {
  const source = {name: "Ada", age: 37, absent: nil}
  const picked = pick(source, ["name", "absent", "name"])
  assert_eq(picked, {name: "Ada", absent: nil})
  assert_eq(pick(source, []), {})
}
```

## What the checker knows about the result

The result has the type of the fields you picked. What the checker can
promise depends on how you write `keys`.

| `keys` | Result type |
|---|---|
| A list literal such as `["name", "age"]` | Those fields, with their original types. Optional fields stay optional |
| A `const` that holds a list literal, or an alias of that `const` | The same as the literal |
| A `const` string inside a list literal | The field that string names |
| A list only known at runtime | Every field it could name, all optional |

A runtime list can be empty, so the checker can't promise that any field is
present. When the list has the type `list<"name" | "age">`, only those two
fields appear in the result, and both are optional.

```harn
type Person = {name: string, age?: int}

type Partial = {name?: string, age?: int}

fn some_fields(person: Person, keys: list<string>) -> Partial {
  return pick(person, keys)
}

fn main(harness: Harness) {
  const fields = ["name"]
  const result: {name: string} = pick({name: "Ada", age: 37}, fields)
  assert_eq(result.name, "Ada")
}
```

The checker reports two mistakes. A literal key that the source type doesn't
have:

```text
pick: unknown field `fss` in `Harness`
```

And reading a field you didn't pick, such as `ctx.tools` after
`pick(harness, ["env", "fs"])`:

```text
field `tools` does not exist on shape `{fs: HarnessFs, env: HarnessEnv}`
```

### Source types

| Source type | Result |
|---|---|
| A record such as `{name: string, age?: int}` | The picked fields, with their types |
| A dictionary such as `dict<string, int>` | Optional fields of type `int`, because the dictionary may not hold them |
| An open record such as `{name: string, ...dict<string, int>}` | Declared fields keep their types. Other keys are optional `int` |
| A type alias or a generic struct | Resolved first, then picked like a record |
| A union such as `{kind: "text", value: string} \| {kind: "number", value: int}` | Picked branch by branch, so `kind` and `value` stay linked. A literal key must exist in every branch |
| An intersection such as `{name: string} & {age: int}` | The combined fields, then picked like a record |

A function or a local value named `pick` shadows the builtin. Passing `pick`
as a value instead of calling it gives the return type `dict<string, unknown>`.

## Pipe form

Use `_` for the source:

```harn
fn main(harness: Harness) {
  const ctx = harness |> pick(_, ["env", "fs"])
  assert_eq(type_of(ctx.fs), type_of(harness.fs))
}
```

## Related helpers

- [`pick_keys(data, keys, {drop_nil: true})`](modules.md#stdcollections) picks
  and then drops `nil` values. It returns a dictionary.
- [`omit` and `merge`](modules.md#stdjson) remove or combine fields.
- `std/json.pick` is gone. See
  [Migrating to 0.10](migrations/v0.10.md#stdjsonpick-is-now-the-global-pick).
