- VM: a fixed-arity `match` list pattern is now exact-length. `match xs { [a, b] -> ... }` matches only
  two-element lists; previously it matched any list of length >= 2 (binding the first two and silently
  dropping the rest). Use the new `[a, ...rest]` form for at-least-N matching.
