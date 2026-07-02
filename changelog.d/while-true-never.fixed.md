- `while true { ... }` with no `break` binding to the loop now types as
  `never`: a function whose tail is such a loop no longer demands an
  unreachable trailing `return`, and statements after the loop are flagged
  unreachable. Adding a `break` at the loop's level restores the fall-through
  return requirement.
