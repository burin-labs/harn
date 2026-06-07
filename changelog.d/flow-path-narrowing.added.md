- **Flow-sensitive type refinement now narrows reference paths and
  `if`-expression branches, at parity with bare-variable narrowing.** Every
  refinement form that narrowed a variable now also narrows an *identifier-
  rooted reference path* — a chain of constant `.`/`?.` property accesses and
  constant `[…]` subscripts (`entry.arguments`, `cfg.opts.mode`, `xs[0]`,
  `m["k"]`):
  - `type_of(path) == "T"`, `path != nil`, and a bare `if path` (truthiness)
    narrow the path; a path whose type is the top type (`unknown`/`any`, e.g. a
    `json_parse` / `llm_call` boundary field) narrows to the tested kind.
  - `schema_is(path, S)` / `is_type(path, S)` and `path.has("k")` narrow the
    path.
  - A tagged-shape-union discriminant narrows the object path
    (`o.msg.kind == "ping"` narrows `o.msg`), gated so it never mangles a
    `dict`/`unknown` object.
  - `match type_of(subject) { "T" -> … }` now narrows the subject — variable
    **or** path — in each arm (previously this narrowed nothing, even for a
    bare variable).
  - The `unknown`-exhaustiveness lint (incomplete `type_of` chain reaching
    `unreachable()` / `throw`) now also covers `unknown`-typed paths.
  - An `if`/`else` used as an expression now narrows its branches like the
    ternary, so `let xs = if type_of(p) == "list" { p } else { [] }` infers
    `list` rather than widening back to `list?`.

  Narrowing is dropped when the base variable or path is reassigned. A
  *dynamic* subscript (`xs[i]` with a non-literal index) is intentionally never
  narrowed — it is not a stable reference. This is a static type-checker
  feature: the runtime is dynamically typed and `type_of` always reflects the
  concrete value, so no runtime change is needed.
