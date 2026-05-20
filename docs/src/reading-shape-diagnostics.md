# Reading shape diagnostics

Harn's typechecker and runtime aim to fail loudly at the closest point to
the bug. Most authoring mistakes around shapes, structs, schemas, and
nilable values surface as one of the diagnostic shapes documented here.

> Looking for the full list of `HARN-<CAT>-<NNN>` codes, their repair ids,
> and safety classes? See the generated [diagnostic codes
> catalog](./diagnostics.md) (or the JSON sidecar at
> [`docs/diagnostics-catalog.json`](https://github.com/burin-labs/harn/blob/main/docs/diagnostics-catalog.json)).
> This page stays focused on the most common shape and nilable patterns.

When you hit one of these, the message tells you the *contract* (what the
type declares), the *observed value* (what arrived), and the *fix*
(`?.`, `assert_shape`, `if x != nil`, etc.) directly. You should rarely
need to dig into the source to interpret one.

## Missing field on a shape annotation

```harn
type User = {name: string, email: string, age: int}

let u: User = {name: "Ada", email: "ada@x", age: 36}
log(u.emial)
```

```text
error: field `emial` does not exist on shape `{name: string, email: string, age: int}` — did you mean `email`?
   |
 5 |   log(u.emial)
   = help: available fields: name, email, age
```

The diagnostic includes:

* **The expected shape** — useful when the alias resolves to an inline shape.
* **A `did you mean` suggestion** when a field name is one or two edits away.
* **The full set of available fields** so you don't have to chase the
  type alias back to its definition.

The same diagnostic fires for struct fields:

```text
error: field `emial` does not exist on struct `User` — did you mean `email`?
   = help: available fields: name, email
```

## Property access on a nil value

A value whose static type is exactly `nil` cannot have fields read off it.
The error explicitly tells you the type is nil and points at the two
canonical fixes — optional access and the nil-guard:

```text
error: cannot access property `foo` on `nil`; the value is statically known to be nil here
   = help: use the optional access operator `?.foo`, or narrow the value with a `!= nil` guard before reading fields
```

## Property access on a nilable type

A `T?` (i.e. `T | nil`) value may be nil at runtime. Direct field access
produces the same fix-suggestion as the nil case:

```text
error: cannot access property `name` on nilable type `{name: string, email: string}?`; the value may be nil at runtime
   = help: use the optional access operator `?.name`, or narrow the value with a `!= nil` guard to drop the nil arm
```

The canonical guard pattern, which lets the typechecker narrow the inner
shape inside the `if` body, is:

```harn
let data = r.data       // r.data: T | nil
if data != nil {
  log(data.name)    // data: T here
}
```

## Property access on an `unknown` value

`unknown` is the safe top type — its whole point is to force a narrowing
step before fields can be read. The diagnostic surfaces as a warning so
gradual code keeps running while still telling the author what to do:

```text
warning: property access `.verdict` on an `unknown` value will fail at runtime if the value is not a shape with that field
   = help: narrow with `is_a`/`type_of`, validate with `assert_shape`, or annotate with a shape type before accessing fields
```

The `schema_is(value, Shape)` form participates in flow narrowing — inside
the truthy branch, `value` narrows to `Shape` and field access is
strict-checked against it. See [Schema as type](./migrations/schema-as-type.md).

## Loose dict literals stay lenient

`let d = {a: 1, b: 2}` is inferred as a structural shape, but historically
this idiom also covers "I want a dynamic dict." Harn keeps it lenient:
a missing field on a literal-inferred shape returns nil at runtime
instead of erroring. To opt into strict checking, annotate the binding
or thread it through a typed function parameter:

```harn
// Lenient — d.missing returns nil
let d = {a: 1, b: 2}

// Strict — d.missing is a typecheck error
type Counts = {a: int, b: int}
let d: Counts = {a: 1, b: 2}
```

The same opt-in applies to `var x = nil` widening loops; annotating with
`var x: T? = nil` enables the strict diagnostics. Use the unannotated
form for genuinely dynamic loops, and the annotated form when you want
the typechecker to enforce the invariant.

## Stdlib helper option errors

Stdlib collection helpers — `pick_keys`, `filter_nil`, `merge`, `pick`,
`omit` — declare typed signatures (`dict<string, V>`, typed option
shapes like `PickKeysOptions = {drop_nil?: bool}`). The typechecker
catches misuse statically against the declared contract:

```text
error: function `pick_keys` parameter `d`: expected dict<string, V>, found string
```

For option-bag parameters, direct dict literals also reject unknown keys:

```text
error: argument 3 `options`: unknown option `dropnil`; expected one of `drop_nil` — did you mean `drop_nil`?
```

When a value flows in dynamically (e.g. via `unknown` or a boundary
source), the runtime parameter guard catches it with the parameter name
intact:

```text
Type error: parameter 'd': expected dict or struct, got string
```

Either way the failure points at the helper boundary, not several
frames down inside the helper body.

## Runtime shape validation

A runtime `assert_shape` (used by typed-parameter checks at `fn` and
`pipeline` boundaries) reports the missing field, the closest match,
and the keys actually present:

```text
Type error: parameter 'u': missing field 'age' (int) — available fields: name, email, agee
```

The "available fields: …" tail is especially useful when an external
boundary (LLM output, JSON parse, host bridge call) silently returned a
near-miss of the expected shape — reading the actual keys often
diagnoses the bug without rerunning under a debugger. The wording
matches the static typechecker so authors see the same phrasing whether
the failure surfaces at `harn check` time or runtime shape validation.
