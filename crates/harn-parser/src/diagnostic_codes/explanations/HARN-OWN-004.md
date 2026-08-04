# HARN-OWN-004 — unvalidated boundary value is used directly

## How to fix

- Validate values returned by boundary APIs such as `json_parse`,
  `llm_call`, and `llm_completion` before accessing fields or indexes.
- Prefer a typed result schema at the call site when the boundary supports it,
  or pass the value through `schema_expect()` / guard it with `schema_is()`
  before property or subscript access.
- Add a shape annotation when the producer is intentionally dynamic but the
  consumer needs a fixed contract.
