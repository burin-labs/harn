# Error handling

Harn provides `try`/`catch`/`throw` for error handling and `retry` for automatic recovery.

## throw

Any value can be thrown as an error:

```javascript
throw "something went wrong"
throw {code: 404, message: "not found"}
throw 42
```

## try/catch

Catch errors with an optional error binding:

```javascript
try {
  let data = json_parse(raw_input)
} catch (e) {
  log("Parse failed: ${e}")
}
```

The error variable is optional:

```javascript
try {
  risky_operation()
} catch {
  log("Something failed, moving on")
}
```

### What gets bound to the error variable

- If the error was created with `throw`: `e` is the thrown value directly (string, dict, etc.)
- If the error is an internal runtime error: `e` is the error's description as a string

### return inside try

A `return` statement inside a `try` block is **not** caught. It propagates
out of the enclosing pipeline or function as expected.

```javascript
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

```javascript
enum AppError {
  NotFound(resource)
  Unauthorized(reason)
  Internal(message)
}

try {
  throw AppError.NotFound("user:123")
} catch (e: AppError) {
  match e.variant {
    "NotFound" -> { log("Missing: ${e.fields[0]}") }
    "Unauthorized" -> { log("Access denied") }
  }
}
```

Errors that don't match the typed catch propagate up the call stack.

## retry

Automatically retry a block up to N times:

```javascript
retry 3 {
  let response = http_post(url, payload)
  let parsed = json_parse(response)
  parsed
}
```

- If the body succeeds on any attempt, returns that result immediately
- If all attempts fail, returns `nil`
- `return` inside a retry block propagates out (not retried)

## Result type

The built-in `Result` enum provides an alternative to try/catch for
representing success and failure as values. A `Result` is either
`Ok(value)` or `Err(error)`.

```javascript
let ok = Ok(42)
let err = Err("something failed")

log(ok)   // Result.Ok(42)
log(err)  // Result.Err(something failed)
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

```javascript
let r = Ok(42)
log(is_ok(r))           // true
log(is_err(r))          // false
log(unwrap(r))          // 42
log(unwrap_or(Err("x"), "default"))  // default
```

### Pattern matching on Result

Result values can be destructured with `match`:

```javascript
fn fetch_data(url) {
  // ... returns Ok(data) or Err(message)
}

match fetch_data("/api/users") {
  Result.Ok(data) -> { log("Got ${len(data)} users") }
  Result.Err(err) -> { log("Failed: ${err}") }
}
```

### The `?` operator

The postfix `?` operator provides concise error propagation. Applied to a
`Result` value, it unwraps `Ok` and returns the value, or immediately
returns the `Err` from the enclosing function.

```javascript
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

```javascript
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

```javascript
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

```javascript
retry 3 {
  try {
    let result = llm_call(prompt, system)
    let parsed = json_parse(result)
    return parsed
  } catch (e) {
    log("Attempt failed: ${e}")
    throw e  // re-throw to trigger retry
  }
}
```
