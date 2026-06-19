- **`harn check` now type-checks expressions inside string interpolation.**
  Holes in `"... ${expr} ..."` are re-parsed and run through the normal
  checker, so undefined calls, argument-type mismatches, and other static
  errors inside `${...}` are caught at check time instead of slipping through
  to a runtime crash.
