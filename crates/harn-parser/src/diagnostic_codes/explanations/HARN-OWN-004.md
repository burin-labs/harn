# HARN-OWN-004 — unvalidated boundary value is used directly

## How to fix

- Validate values returned by boundary APIs such as `json_parse`,
  `llm_call`, and `llm_completion` before accessing fields or indexes.
- Prefer a typed result schema at the call site when the boundary supports it,
  or pass the value through `schema_expect()` / guard it with `schema_is()`
  before property or subscript access.
- A type annotation on the binding also validates: since harn#6252 a declared
  type is checked where it is written, exactly as a declared parameter type is
  checked where it is passed. `const doc: {name: string} = json_parse(text)`
  rejects a payload whose `name` is not a string. Constructing a struct with
  annotated fields (harn#6268) checks those fields the same way.
- Choose between them by the report you want on failure. A binding assertion
  names the binding and the declared type; `schema_expect()` names the field
  that failed and why. For a payload from outside the program, the second is
  usually worth the extra line.
