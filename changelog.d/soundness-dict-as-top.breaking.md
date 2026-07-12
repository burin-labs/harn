- **A bare `dict` no longer satisfies a specific shape type without narrowing.**
  Assigning a `dict` (or passing it) where a shape like `{name: string}` is
  expected used to be accepted silently — the hole that let unvalidated
  `json_parse` output flow into a typed record and fail only at runtime. `dict`
  now behaves like `unknown` at a shape boundary: narrow it first with
  `schema_is` / `schema_expect` / `.has(...)`, exactly as you already narrow
  `unknown`. A shape still widens to `dict` (a shape *is* a dict), which is
  sound and unchanged.

  The empty literal `{}` is the sole exception: it is the top object type
  (matching TS/Flow `{}`), so it satisfies an all-optional shape and a
  `let m = {}` still accepts a later `m = some_dict()`. A dict literal is
  otherwise typed by its precise fields, checked structurally against the
  target shape.
