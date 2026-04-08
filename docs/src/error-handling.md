# Error handling

Harn provides `try`/`catch`/`throw` for error handling and `retry` for automatic recovery.

## throw

Any value can be thrown as an error:

```harn
throw "something went wrong"
throw {code: 404, message: "not found"}
throw 42
```

## try/catch

Catch errors with an optional error binding:

```harn
try {
  let data = json_parse(raw_input)
} catch (e) {
  println("Parse failed: ${e}")
}
```

The error variable is optional:

```harn
try {
  risky_operation()
} catch {
  println("Something failed, moving on")
}
```

### What gets bound to the error variable

- If the error was created with `throw`: `e` is the thrown value directly (string, dict, etc.)
- If the error is an internal runtime error: `e` is the error's description as a string

### return inside try

A `return` statement inside a `try` block is **not** caught. It propagates
out of the enclosing pipeline or function as expected.

```harn
fn find_user(id) {
  try {
    let user = lookup(id)
    return user  // this returns from find_user, not caught
  } catch (e) {
    return nil
  }
}
```

## Typed catch

Catch specific error types using enum-based error hierarchies:

```harn
enum AppError {
  NotFound(resource)
  Unauthorized(reason)
  Internal(message)
}

try {
  throw AppError.NotFound("user:123")
} catch (e: AppError) {
  match e.variant {
    "NotFound" -> { println("Missing: ${e.fields[0]}") }
    "Unauthorized" -> { println("Access denied") }
  }
}
```

Errors that don't match the typed catch propagate up the call stack.

## require

The `require` statement checks a condition and throws an error if it is
false. An optional second argument provides the error message:

```harn
require len(items) > 0, "items list must not be empty"
require user != nil, "user is required"
require score >= 0    // throws a generic error if false
```

`require` is useful at the top of a function to validate preconditions
before proceeding. If the condition is falsy, execution stops with a
thrown error that can be caught by `try`/`catch` or will surface as a
runtime error.

## guard

The `guard` statement provides an early-return pattern. If the condition
is false, the `else` block executes. The `else` block must exit the
current scope (typically via `return` or `throw`):

```harn
fn process(input) {
  guard input != nil else {
    return "no input"
  }
  guard type_of(input) == "string" else {
    throw "expected string, got ${type_of(input)}"
  }
  // input is guaranteed non-nil and a string here
  return input.uppercase()
}
```

After a `guard` statement, the type checker narrows the variable's type
based on the condition. For example, `guard x != nil` ensures `x` is
non-nil in subsequent code.

## retry

Automatically retry a block up to N times:

```harn
retry 3 {
  let response = http_post(url, payload)
  let parsed = json_parse(response)
  parsed
}
```

- If the body succeeds on any attempt, returns that result immediately
- If all attempts fail, returns `nil`
- `return` inside a retry block propagates out (not retried)

## Try-expression

The `try` keyword without a `catch` block acts as a try-expression. It
evaluates the body and returns a `Result`:

- On success: `Result.Ok(value)`
- On error: `Result.Err(error)`

```harn
let result = try { json_parse(raw_input) }
```

This is useful when you want to capture an error as a value rather than
crashing or needing a full `try`/`catch`:

```harn
let parsed = try { json_parse(input) }
if is_err(parsed) {
  println("Bad input, using defaults")
  parsed = Ok({})
}
let data = unwrap(parsed)
```

The try-expression pairs naturally with the `?` operator. Use `try` to
enter Result-land and `?` to propagate within it:

```harn
fn fetch_json(url) {
  let body = try { http_get(url) }
  let text = unwrap(body)?
  let data = try { json_parse(text) }
  return data
}
```

If a `catch` block follows `try`, it is parsed as the traditional
`try`/`catch` statement -- not a try-expression.

## Runtime shape validation errors

When a function parameter has a structural type annotation (a shape like
`{name: string, age: int}`), Harn validates the argument at runtime. If
the argument is missing a required field or a field has the wrong type,
a clear error is produced:

```harn,ignore
fn process(user: {name: string, age: int}) {
  println("${user.name} is ${user.age}")
}

process({name: "Alice"})
// Error: parameter 'user': missing field 'age' (int)

process({name: "Alice", age: "old"})
// Error: parameter 'user': field 'age' expected int, got string
```

Shape validation works with both plain dicts and struct instances. Extra
fields beyond those listed in the shape are allowed (width subtyping).

This catches a common class of bugs where a dict is passed with missing or
mistyped fields, giving you precise feedback about exactly which field is
wrong.

## Result type

The built-in `Result` enum provides an alternative to try/catch for
representing success and failure as values. A `Result` is either
`Ok(value)` or `Err(error)`.

```harn
let ok = Ok(42)
let err = Err("something failed")

println(ok)   // Result.Ok(42)
println(err)  // Result.Err(something failed)
```

The shorthand constructors `Ok(value)` and `Err(value)` are equivalent to
`Result.Ok(value)` and `Result.Err(value)`.

### Result helper functions

| Function | Description |
|---|---|
| `is_ok(r)` | Returns `true` if `r` is `Result.Ok` |
| `is_err(r)` | Returns `true` if `r` is `Result.Err` |
| `unwrap(r)` | Returns the `Ok` value, throws if `r` is `Err` |
| `unwrap_or(r, default)` | Returns the `Ok` value, or `default` if `r` is `Err` |
| `unwrap_err(r)` | Returns the `Err` value, throws if `r` is `Ok` |

```harn
let r = Ok(42)
println(is_ok(r))           // true
println(is_err(r))          // false
println(unwrap(r))          // 42
println(unwrap_or(Err("x"), "default"))  // default
```

### Pattern matching on Result

Result values can be destructured with `match`:

```harn
fn fetch_data(url) {
  // ... returns Ok(data) or Err(message)
}

match fetch_data("/api/users") {
  Result.Ok(data) -> { println("Got ${len(data)} users") }
  Result.Err(err) -> { println("Failed: ${err}") }
}
```

### The `?` operator

The postfix `?` operator provides concise error propagation. Applied to a
`Result` value, it unwraps `Ok` and returns the value, or immediately
returns the `Err` from the enclosing function.

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

let r1 = compute(20)  // Result.Ok(20)
let r2 = compute(0)   // Result.Err(division by zero)
```

The `?` operator has the same precedence as `.`, `[]`, and `()`, so it
chains naturally:

```harn
fn fetch_and_parse(url) {
  let response = http_get(url)?
  let data = json_parse(response)?
  return Ok(data)
}
```

Applying `?` to a non-Result value produces a runtime type error.

### Result vs. try/catch

Use `Result` and `?` when errors are expected outcomes that callers should
handle (validation failures, missing data, parse errors). Use `try`/`catch`
for unexpected errors or when you want to recover from failures in-place
without propagating them through return values.

The two patterns can be combined:

```harn
fn safe_parse(input) {
  try {
    let data = json_parse(input)
    return Ok(data)
  } catch (e) {
    return Err("parse error: ${e}")
  }
}

fn process(raw) {
  let data = safe_parse(raw)?   // propagate Err if parse fails
  return Ok(transform(data))
}
```

## Stack traces

When a runtime error occurs, Harn displays a stack trace showing the call
chain that led to the error. The trace includes file location, source
context, and the sequence of function calls.

```text
error: division by zero
  --> example.harn:3:14
  |
3 |   let x = a / b
  |              ^
  = note: called from compute at example.harn:8
  = note: called from pipeline at example.harn:12
```

The error format shows:

- **Error message**: what went wrong
- **Source location**: file, line, and column where the error occurred
- **Source context**: the relevant source line with a caret (`^`) pointing
  to the exact position
- **Call chain**: each function in the call stack, from innermost to
  outermost, with file and line numbers

Stack traces are captured at the point of the error, before try/catch
unwinding, so the full call chain is preserved even when errors are caught
at a higher level.

## Combining patterns

```harn
retry 3 {
  try {
    let result = llm_call(prompt, system)
    let parsed = json_parse(result)
    return parsed
  } catch (e) {
    println("Attempt failed: ${e}")
    throw e  // re-throw to trigger retry
  }
}
```
