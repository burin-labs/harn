## Enums

Enums define a type with a fixed set of named variants, each optionally
carrying associated data.

### Enum declaration

```harn
enum Color {
  Red,
  Green,
  Blue
}

enum Shape {
  Circle(float),
  Rectangle(float, float)
}
```

Variants without data are simple tags. Variants with data carry positional
fields specified in parentheses.

### Enum construction

Variants are constructed using dot syntax on the enum name:

```harn
let c = Color.Red
let s = Shape.Circle(5.0)
let r = Shape.Rectangle(3.0, 4.0)
```

### Pattern matching on enums

Enum variants are matched using `EnumName.Variant(binding)` patterns in
`match` expressions:

```harn
match s {
  Shape.Circle(radius) -> { log("circle r=${radius}") }
  Shape.Rectangle(w, h) -> { log("rect ${w}x${h}") }
}
```

A `match` on an enum **must** be exhaustive: a missing variant is a hard
error, not a warning. Add the missing arm or end with a wildcard
`_ -> { … }` arm to opt out. `if/elif/else` chains stay intentionally
partial; opt into exhaustiveness by ending the chain with
`unreachable("…")`.

### Built-in result enum

Harn provides a built-in generic `Result<T, E>` enum with two variants:

- `Result.Ok(value)` -- represents a successful result
- `Result.Err(error)` -- represents an error

Shorthand constructor functions `Ok(value)` and `Err(value)` are available
as builtins, equivalent to `Result.Ok(value)` and `Result.Err(value)`.

```harn
let ok = Ok(42)
let err = Err("something failed")
let typed_ok: Result<int, string> = ok

// Equivalent long form:
let ok2 = Result.Ok(42)
let err2 = Result.Err("oops")
```

### Result helper functions

| Function | Description |
|---|---|
| `is_ok(r)` | Returns `true` if `r` is `Result.Ok` |
| `is_err(r)` | Returns `true` if `r` is `Result.Err` |
| `unwrap(r)` | Returns the `Ok` value, throws if `r` is `Err` |
| `unwrap_or(r, default)` | Returns the `Ok` value, or `default` if `r` is `Err` |
| `unwrap_err(r)` | Returns the `Err` value, throws if `r` is `Ok` |

### The `?` operator (result propagation)

The postfix `?` operator unwraps a `Result.Ok` value or propagates a
`Result.Err` from the current function. It is a postfix operator with the
same precedence as `.`, `[]`, and `()`.

```harn
fn divide(a, b) {
  if b == 0 {
    return Err("division by zero")
  }
  return Ok(a / b)
}

fn compute(x) {
  let result = divide(x, 2)?   // unwraps Ok, or returns Err early
  return Ok(result + 10)
}

let r1 = compute(20)   // Result.Ok(20)
let r2 = compute(0)    // would propagate Err from divide
```

The `?` operator requires its operand to be a `Result` value. Applying `?`
to a non-Result value produces a type error at runtime.

Disambiguation: when the parser sees `expr?`, it distinguishes between the
postfix `?` (Result propagation) and the ternary `? :` operator by checking
whether the token following `?` could start a ternary branch expression.
For `expr?[...]`, optional subscript is used unless the `?` is followed by a
valid branch expression and a top-level ternary `:`. For example,
`repo ? ["--repo", repo] : []` parses as a ternary without extra parentheses.

### Pattern matching on result

```harn
match result {
  Result.Ok(val) -> { log("success: ${val}") }
  Result.Err(err) -> { log("error: ${err}") }
}
```

### Try-expression

The `try` keyword used without a `catch` block acts as a try-expression.
It evaluates the body and wraps the result in a `Result`:

- If the body succeeds, returns `Result.Ok(value)`.
- If the body succeeds with an existing `Result`, returns that `Result`
  unchanged instead of nesting it as `Result.Ok(Result.Ok(...))`.
- If the body throws an error, returns `Result.Err(error)`.

```harn
let result = try { json_parse(raw_input) }
// result is Result.Ok(parsed_data) or Result.Err("invalid JSON: ...")

let checked = try { schema_check(data, schema) }
// checked is schema_check's Result directly, not Result.Ok(Result.Ok(...))
```

The try-expression is the complement of the `?` operator: `try` enters
Result-land by catching errors, while `?` exits Result-land by propagating
errors. Together they form a complete error-handling pipeline:

```harn
fn safe_divide(a, b) {
  let result = try { a / b }
  return result
}

fn compute(x) {
  let val = safe_divide(x, 2)?  // unwrap Ok or propagate Err
  return Ok(val + 10)
}
```

No `catch` or `finally` block is needed for the `Result`-wrapping form. When
`catch` or `finally` follow `try`, the form is a handled `try`/`catch`
expression whose value is the try or catch body's tail value (see
[try/catch/finally](#trycatchfinally)); only the bare `try { ... }` form wraps
in `Result`.

### Result in pipelines

The `?` operator works naturally in pipelines:

```harn
fn fetch_and_parse(url) {
  let response = http_get(url)?
  let data = json_parse(response)?
  return Ok(data)
}
```

