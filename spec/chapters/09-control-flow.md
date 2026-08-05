## Control flow

### if/else

```harn
if condition {
  // then
} else if other {
  // else-if
} else {
  // else
}
```

`else if` chains are parsed as a nested `ifElse` node in the else branch.

### for/in

```harn
for item in iterable {
  // body
}
```

If `iterable` is a list, iterates over elements. If `iterable` is a dict, iterates over
entries sorted by key, where each entry is `{key: "...", value: ...}`.
The loop variable is mutable within the loop body.

### while

```harn
while condition {
  // body
}
```

Maximum 10,000 iterations (safety limit). Condition is re-evaluated each iteration.

A `while true { ... }` whose body contains no `break` binding to that loop
types as `never` (see [The `never` type](#the-never-type)): control can only
leave it through `return`/`throw`, so a function whose tail is such a loop
does not need a trailing `return`, and statements after the loop are flagged
unreachable.

### match

`match` is an expression. It can be used as a standalone statement or in
expression position; the selected arm evaluates to the value of its block's
last expression.

```harn
match value {
  pattern1 -> { body1 }
  pattern2 if condition -> { body2 }
}
```

Patterns are expressions. Each pattern is evaluated and compared to the match value
using `valuesEqual`. An arm may include an `if` guard after the pattern; when
present, the arm only matches if the pattern matches **and** the guard expression
evaluates to a truthy value. The first matching arm executes.

If no arm matches, a runtime error is thrown (`No match arm matched the value`).
This makes non-exhaustive matches a hard failure rather than a silent `nil`.

```harn
const x = 5
const label = match x {
  1 -> { "one" }
  n if n > 3 -> { "big: ${n}" }
  _ -> { "other" }
}
// label == "big: 5"
```

#### List patterns

A list pattern `[p0, p1, …]` matches a list of **exactly** that length and binds
each identifier element to the value at its position (literals are compared,
`_` discards). A trailing `...rest` element makes the pattern match a list of
**at least** the leading arity and binds the remainder as a new list (`..._`
matches the tail without binding it):

```harn
match coords {
  [x, y] -> { "2d: ${x},${y}" }          // matches ONLY length 2
  [x, y, z] -> { "3d" }                   // matches ONLY length 3
  [first, ...rest] -> { "${first}+${rest}" } // length >= 1; rest is a list
  [] -> { "empty" }
  _ -> { "other" }
}
```

When the matched value is a `list<T>`, the leading bindings have type `T` and a
`...rest` binding has type `list<T>`. Only one `...rest` is allowed and it must
be last.

### retry

```harn
retry 3 {
  // body that may throw
}
```

Executes the body up to N times. If the body succeeds (no error), returns immediately.
If the body throws, catches the error and retries. `return` statements inside retry
propagate out (are not retried). After all attempts are exhausted, returns `nil`
(does not re-throw the last error).

