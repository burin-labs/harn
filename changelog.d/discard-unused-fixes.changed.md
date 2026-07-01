- **Unused-variable lint fixes now use the Harn discard binding (#3783).** The linter
  and editor quick fix suggest replacing unused bindings with `_` instead of
  inventing underscore-prefixed names, matching the language's existing discard
  parameter and binding semantics.
