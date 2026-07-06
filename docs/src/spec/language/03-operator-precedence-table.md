<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Operator precedence table

From lowest to highest binding:

| Precedence | Operators | Associativity | Description |
|---|---|---|---|
| 1 | `\|>` | Left | Pipe |
| 2 | `? :` | Right | Ternary conditional |
| 3 | `\|\|` | Left | Logical OR |
| 4 | `&&` | Left | Logical AND |
| 5 | `==` `!=` | Left | Equality |
| 6 | `<` `>` `<=` `>=` `in` `not in` | Left | Comparison / membership |
| 7 | `+` `-` | Left | Additive |
| 8 | `??` | Left | Nil coalescing |
| 9 | `*` `/` `%` | Left | Multiplicative |
| 10 | `!` `-` (unary) | Right (prefix) | Unary |
| 11 | `**` | Right | Exponentiation |
| 12 | `.` `?.` `[]` `?.[]` `[:]` `()` `?` | Left | Postfix |

Exponentiation binds more tightly than a unary prefix on its **left** operand,
so `-2 ** 2` parses as `-(2 ** 2)` (`-4`), matching Python, Ruby, and ordinary
math notation rather than the spreadsheet `(-2) ** 2` reading. The **right**
(exponent) operand still accepts a unary prefix, so `2 ** -3` is `2 ** (-3)`,
and chained `**` remains right-associative (`2 ** 3 ** 2` is `2 ** (3 ** 2)`).

### Multiline expressions

Binary operators `|>`, `||`, `&&`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `??`,
`+`, `*`, `/`, `%`, `**`, and the `.` / `?.` member access operators can span
multiple lines. The operator at the start of a continuation line causes the
parser to treat it as a continuation of the previous expression rather than a
new statement.

Note: `-` does not support multiline continuation because it is also a unary
negation prefix. Keyword operators `in`, `not in`, and `to` also require an
explicit backslash continuation.

```harn,ignore
let result = items
  .filter({ x -> x > 0 })
  .map({ x -> x * 2 })

let msg = "hello"
  + " "
  + "world"

let ok = check_a()
  && check_b()
  || fallback()
```

### Pipe placeholder (`_`)

When the right side of `|>` contains `_` identifiers, the expression is
automatically wrapped in a closure where `_` is replaced with the piped
value:

```harn
"hello world" |> split(_, " ")     // desugars to: |> { __pipe -> split(__pipe, " ") }
[3, 1, 2] |> _.sort()             // desugars to: |> { __pipe -> __pipe.sort() }
items |> len(_)                    // desugars to: |> { __pipe -> len(__pipe) }
```

Without `_`, the pipe passes the value as the sole argument to the callable on
the right side. Use `_` whenever the piped value should be placed inside a
larger expression or a specific argument position.
