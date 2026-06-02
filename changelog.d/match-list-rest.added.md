- VM: `match` list patterns now support a trailing `...rest` element, mirroring `let`-destructuring.
  `match xs { [head, ...tail] -> ... }` matches any list with at least one element, binds `head` to the
  first and `tail` to a new list of the remainder (typed `list<T>`); `[a, ..._]` matches at-least-N and
  discards the tail.
