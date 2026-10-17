<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Operator precedence table

The table runs from lowest to highest binding. Operators with higher binding
group first. For example, `a && b || c` parses as `(a && b) || c` because `&&`
binds more tightly than `||`.

<!-- operator-precedence:begin -->
| Precedence | Operators | Associativity | Description |
|---|---|---|---|
| 1 | `\|>` | Left | Pipe |
| 2 | `? :` | Right | Ternary conditional |
| 3 | `\|\|` | Left | Logical OR |
| 4 | `&&` | Left | Logical AND |
| 5 | `==` `!=` | Left | Equality |
| 6 | `<` `>` `<=` `>=` `in` `not in` | Left | Comparison / membership |
| 7 | `??` | Left | Nil coalescing |
| 8 | `..` `..=` | None | Range formation |
| 9 | `+` `-` | Left | Additive |
| 10 | `*` `/` `%` | Left | Multiplicative |
| 11 | `!` `-` | Right | Unary prefix |
| 12 | `**` | Right | Exponentiation |
| 13 | `.` `?.` `[]` `?.[]` `[:]` `()` `?` `!` | Left | Postfix |
<!-- operator-precedence:end -->

The table is generated from `harn_parser::operators::Precedence` by
`make gen-operator-precedence`, which also writes the tree-sitter grammar's
precedence constants, so the parser, formatter, editor grammar, and this
table cannot disagree.

`??` and range formation follow Swift's `NilCoalescingPrecedence` and
`RangeFormationPrecedence`: `??` binds looser than arithmetic and range
formation and tighter than comparison, so `count ?? 0 + 1` is
`count ?? (0 + 1)` and `count ?? 0 == 1` is `(count ?? 0) == 1`. Swift makes
`??` right-associative; Harn groups it to the left, which yields the same
value because `??` returns its first non-nil operand. Ranges do not chain:
`a..b..c` is a parse error.

`!` appears at two precedence levels with two distinct meanings: a **prefix**
`!` (level 11) is logical negation (`!ok`), while a **postfix** `!` (level 13)
is the non-null assertion (`value!`). They never collide — a `!` before an
operand is negation, a `!` after one is the assertion — and `!=` is a single
token, so neither reading is ambiguous.

Exponentiation binds more tightly than a unary prefix on its **left** operand,
so `-2 ** 2` parses as `-(2 ** 2)` (`-4`), matching Python, Ruby, and ordinary
math notation rather than the spreadsheet `(-2) ** 2` reading. The **right**
(exponent) operand still accepts a unary prefix, so `2 ** -3` is `2 ** (-3)`,
and chained `**` remains right-associative (`2 ** 3 ** 2` is `2 ** (3 ** 2)`).

### Multiline expressions

Binary operators `|>`, `||`, `&&`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `??`,
`..`, `..=`, `+`, `*`, `/`, `%`, `**`, and the `.` / `?.` member access
operators can span multiple lines. The operator at the start of a continuation line causes the
parser to treat it as a continuation of the previous expression rather than a
new statement.

Note: `-` does not support multiline continuation because it is also a unary
negation prefix. Keyword operators `in` and `not in` also require an explicit
backslash continuation.

```harn,ignore
const result = items
  .filter({ x -> x > 0 })
  .map({ x -> x * 2 })

const msg = "hello"
  + " "
  + "world"

const ok = check_a()
  && check_b()
  || fallback()
```

### Pipe placeholder (`_`)

When the right side of `|>` contains `_` identifiers, the expression is
automatically wrapped in a closure where `_` is replaced with the piped
value:

```harn
// desugars to: |> { __pipe -> split(__pipe, " ") }
"hello world" |> split(_, " ")
// desugars to: |> { __pipe -> __pipe.sorted() }
[3, 1, 2] |> _.sorted()
// desugars to: |> { __pipe -> len(__pipe) }
items |> len(_)
```

Without `_`, the pipe passes the value as the sole argument to the callable on
the right side. Use `_` whenever the piped value should be placed inside a
larger expression or a specific argument position.
