- **Typechecker: `schema_is(x, S)` on a shape value no longer drops fields the
  variable was already known to have.** When `x` was typed as a shape, the
  truthy branch previously narrowed to the schema's shape verbatim, discarding
  every field the existing annotation declared — so e.g. `if schema_is(x, {b:
  string}) { x.a }` on `x: {a: int, b: string}` falsely reported `a` missing.
  Width subtyping says the value still has those fields after the check, so the
  intersection now keeps the current shape's fields, intersects overlapping
  field types, and appends schema-only required fields that the matched check
  proves are present.
