# Replace `input?.x ?? default` blocks with destructuring

The single most repeated shape in Harn-using code is optional-field extraction
with a fallback:

```harn,ignore
let input = pipeline_input() ?? {}
let path = input?.path ?? ""
let namespace = input?.namespace ?? nil
let retries = input?.retries ?? 0
let opts = input?.opts ?? {}
```

Every line repeats the source and the `?.` / `??` dance. Harn supports
[destructuring with defaults](../spec/language/05-destructuring-patterns.md#default-values), so the whole
block collapses to one bind:

```harn,ignore
let { namespace = nil, opts = {}, path = "", retries = 0 } = pipeline_input() ?? {}
```

The two forms are equivalent — including the types each binding receives.

## The mechanical rewrite

A run of `let <name> = <src>?.<key> ?? <default>` statements that share the same
`<src>` becomes a single dict pattern over that source:

```harn,ignore
// before
let cfg = load_config() ?? {}
let host = cfg?.host ?? "0.0.0.0"
let port = cfg?.port ?? 8080
let tls  = cfg?.tls  ?? false

// after
let { host = "0.0.0.0", port = 8080, tls = false } = load_config() ?? {}
```

Rules that keep the rewrite behavior-preserving:

- **A missing key binds to its default.** `cfg?.host ?? "0.0.0.0"` and
  `{ host = "0.0.0.0" }` both apply the default when `host` is absent *or* `nil`
  — destructuring uses the same nil-coalescing semantics.
- **A `nil` default stays optional.** `let { namespace = nil } = src` infers the
  same `T | nil` type as `src?.namespace ?? nil`, so downstream nil-checks still
  type-check.
- **Rename when the binding name differs from the key.** `let id = src?.userId`
  becomes `let { userId: id } = src` (the `key: alias` form). Rest collects the
  leftovers: `let { host, ...rest } = src`.
- **Dict-pattern keys are written alphabetically.** `harn fmt` orders them, so
  emit `{ host, port, tls }` not `{ port, host, tls }`.
- **Rest elements take no default.** `...rest` always binds (`{}` / `[]` when
  empty), so it never needs a fallback.

## Type inference matches the hand-written form

The binding types are inferred *exactly* as the `?.` / `??` expression would
produce them, so migrating never loses precision under the type checker:

```harn,ignore
let { port = 8080 } = load_config() ?? {}
let p: int = port        // ok — `port` infers `int` from the default
let q: string = port     // error: expected string, found int
```

When the source is a typed shape, present fields keep their declared type
(`{ host: string }` ⇒ `host: string`); when the source is an untyped dict, the
default's type carries through (`{ port = 8080 }` ⇒ `port: int`).

> **Note:** positional/tuple-precise element types for *list* destructuring
> (`let [a, b = 0] = xs`) are not yet inferred element-by-element — list
> bindings take the homogeneous element type. Dict destructuring, where nearly
> all of the savings are, is fully inferred.

## Migrating at scale

For a whole codebase, drive the rewrite with the AST-precise edit primitives
(see [Structured refactorings](./structured-refactorings.md)) rather than hand
edits: match consecutive `let X = SRC?.K ?? D` statements sharing `SRC`, fold
them into one dict pattern, and let `harn fmt` normalize key order.
