# HARN-OWN-004 — unvalidated boundary value is used directly

## How to fix

- Validate values returned by boundary APIs such as `json_parse`,
  `llm_call`, and `llm_completion` before accessing fields or indexes.
- Prefer a typed result schema at the call site when the boundary supports it,
  or pass the value through `schema_expect()` / guard it with `schema_is()`
  before property or subscript access.
- A type annotation is not a substitute. It is erased before the value exists,
  so `const doc: {name?: string} = json_parse(text)` reads whatever the payload
  actually contains — an `int` out of a field declared `string`, or a JSON array
  where a record was declared — with no diagnostic at compile time or run time.
  Annotating a boundary value states a contract nothing enforces, so it no
  longer clears this diagnostic.
