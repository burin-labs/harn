# HARN-OWN-004 — unvalidated boundary value is used directly

## How to fix

- Validate values returned by boundary APIs such as `json_parse`,
  `llm_call`, and `llm_completion` before accessing fields or indexes.
- Prefer a typed result schema at the call site when the boundary supports it,
  or pass the value through `schema_expect()` / `schema_check()` / guard it
  with `schema_is()` before property or subscript access.
- A shape annotation also clears this diagnostic, but it is checked only
  statically: `const doc: {name?: string} = json_parse(text)` accepts an int in
  `name` at runtime, and accepts a JSON array as the record. Reach for it when
  the shape is already guaranteed by something else, and prefer a schema when
  the payload comes from a source you do not control.
