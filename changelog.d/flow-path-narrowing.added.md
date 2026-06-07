- **Flow-sensitive type refinement now narrows reference paths and
  `if`-expression branches.** `type_of(entry.arguments) == "list"` and
  `entry.arguments != nil` narrow the property path itself (any identifier-
  rooted chain of constant `.` / `?.` accesses), not just bare variables, so a
  guarded path flows into a typed parameter without a defensive `?? []`
  coercion. The same refinements now also apply inside an `if`/`else` used as
  an expression for its value — matching the ternary — so
  `let xs = if type_of(p) == "list" { p } else { [] }` infers `list` instead of
  widening back to `list?`. Path narrowing is dropped when the base variable or
  the path is reassigned.
