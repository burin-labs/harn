## Binary operator semantics

### Arithmetic (`+`, `-`, `*`, `/`)

| Left | Right | `+` | `-` | `*` | `/` |
|---|---|---|---|---|---|
| int | int | int¹ | int¹ | int¹ | int (truncating) |
| float | float | float | float | float | float |
| int | float | float | float | float | float |
| float | int | float | float | float | float |
| string | string | string (concatenation) | **TypeError** | **TypeError** | **TypeError** |
| string | int | **TypeError** | **TypeError** | string (repetition) | **TypeError** |
| int | string | **TypeError** | **TypeError** | string (repetition) | **TypeError** |
| list | list | list (concatenation) | **TypeError** | **TypeError** | **TypeError** |
| dict | dict | dict (merge, right wins) | **TypeError** | **TypeError** | **TypeError** |
| other | other | **TypeError** | **TypeError** | **TypeError** | **TypeError** |

¹ When an `int` result would overflow the 64-bit range, it promotes to
`float` rather than wrapping two's-complement (so `i64::MAX + 1` is a large
`float`, not a negative `int`). This matches `sum`/`abs`, which already promote
on overflow. In-range integer arithmetic stays `int`. The same applies to
unary negation (`-i64::MIN`) and `**` (see below).

Division by zero depends on the result type. Integer division by zero
(`int / int`) raises a runtime error that propagates like any other thrown
value (catchable with `try`/`catch`). Float division by zero follows
IEEE-754: it yields `±inf` (or `NaN` for `0.0 / 0.0`) and never raises — this
applies whenever either operand is a `float`, since the result is a `float`.
`string * int` repeats the string; negative or zero counts return `""`.

Type mismatches that are not listed as valid combinations above produce a
`TypeError` at runtime. The type checker reports these as compile-time errors
when operand types are statically known. Use `to_string()` or string
interpolation (`"${expr}"`) for explicit type conversion.

An operand whose static type is nilable (`int?`, `string | nil`, …) is
rejected at compile time for the arithmetic and concatenation operators
(`+`, `-`, `*`, `/`, `%`, `**`), because `nil + 1` (etc.) faults at runtime.
Flow narrowing applies: a binding proven non-nil by an earlier assignment
(`x = 5`), a `!= nil` guard, or `??` is narrowed to its non-nil type and is
not flagged. Assignment narrowing covers both variables and reference paths
(`obj.field = value` narrows `obj.field`), and is invalidated by a later
reassignment of the binding or its base.

### Modulo (`%`)

`%` is numeric-only. `int % int` returns `int`; any case involving a `float`
returns `float`. Modulo by a zero divisor always raises a runtime error,
regardless of operand types (unlike float division, it never yields `NaN`).

### Exponentiation (`**`)

`**` is numeric-only and right-associative, so `2 ** 3 ** 2` evaluates as
`2 ** (3 ** 2)`.

- `int ** int` returns `int` for non-negative exponents that fit in `u32`,
  promoting to `float` if the result overflows the 64-bit range.
- Negative or very large integer exponents promote to `float`.
- Any case involving a `float` returns `float`.
- Non-numeric operands raise `TypeError`.
- A unary minus on the base binds looser than `**`, so `-2 ** 2` is
  `-(2 ** 2)` (`-4`), not `(-2) ** 2` (`4`). The exponent still accepts a
  unary prefix, so `2 ** -3` is `2 ** (-3)`.

### Logical (`&&`, `||`)

Short-circuit evaluation:

- `&&`: if left is falsy, returns `false` without evaluating right.
- `||`: if left is truthy, returns `true` without evaluating right.

### Nil coalescing (`??`)

Short-circuit: if left is not `nil`, returns left without evaluating right.
`??` binds tighter than additive/comparison/logical operators but looser than
multiplicative operators, so `xs?.count ?? 0 > 0` parses as
`(xs?.count ?? 0) > 0`.
`harn fmt` prints clarifying parentheses when `??` is nested inside looser
binary expressions, so `classified == fixture?.expect_missing_dep ?? false`
formats as `classified == (fixture?.expect_missing_dep ?? false)`.

### Pipe (`|>`)

`a |> f` evaluates `a`, then:

1. If `f` evaluates to a closure, invokes it with `a` as the single argument.
2. If `f` is an identifier resolving to a builtin, calls the builtin with `[a]`.
3. If `f` is an identifier resolving to a closure variable, invokes it with `a`.
4. Otherwise raises a type error.

### Ternary (`? :`)

`condition ? trueExpr : falseExpr` evaluates `condition`, then evaluates and returns
either `trueExpr` (if truthy) or `falseExpr`.

### Ranges (`to`, `to … exclusive`)

`a to b` evaluates `a` and `b` (both must be integers) and produces a list of
consecutive integers. The form is **inclusive** by default — `1 to 5` is
`[1, 2, 3, 4, 5]` — because that matches how the expression reads aloud.

Add the trailing modifier `exclusive` to get the half-open form:
`1 to 5 exclusive` is `[1, 2, 3, 4]`.

| Expression           | Value               | Shape      |
|----------------------|---------------------|------------|
| `1 to 5`             | `[1, 2, 3, 4, 5]`   | `[a, b]`   |
| `1 to 5 exclusive`   | `[1, 2, 3, 4]`      | `[a, b)`   |
| `0 to 3`             | `[0, 1, 2, 3]`      | `[a, b]`   |
| `0 to 3 exclusive`   | `[0, 1, 2]`         | `[a, b)`   |

If `b < a`, the result is the empty list. The `range(n)` / `range(a, b)` stdlib
builtins always produce the half-open form, for Python-compatible indexing.
