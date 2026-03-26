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

A `return` statement inside a `try` block is **not** caught. It propagates out of the enclosing pipeline or function as expected.

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
