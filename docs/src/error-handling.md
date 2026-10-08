# Error handling

Harn provides `try`/`catch`/`throw` for error handling and `retry` for automatic recovery.

> **Reading errors.** Most type-check and runtime errors around shapes,
> structs, schemas, and nilable values follow a small set of patterns
> that point straight at the fix. See
> [Reading shape diagnostics](./reading-shape-diagnostics.md) for a tour,
> or the [Diagnostic codes catalog](./diagnostics.md) for the full
> `HARN-<CAT>-<NNN>` reference.

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
  const data = json_parse(raw_input)
} catch (e) {
  harness.stdio.log("Parse failed at ${e.line}:${e.column}: ${e.message}")
}
```

The error variable is optional:

```harn
fn risky_operation() { throw "boom" }

try {
  risky_operation()
} catch {
  harness.stdio.log("Something failed, moving on")
}
```

### What gets bound to the error variable

- If the error was created with `throw`: `e` is the thrown value directly (string, dict, etc.)
- If the error is an internal runtime error: `e` is the error's description as a string

### return inside try

A `return` statement inside a `try` block is **not** caught. It propagates
out of the enclosing pipeline or function as expected.

```harn,ignore
fn find_user(id) {
  try {
    const user = lookup(id)
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
    "NotFound" -> { harness.stdio.log("Missing: ${e.fields[0]}") }
    "Unauthorized" -> { harness.stdio.log("Access denied") }
    "Internal" -> { harness.stdio.log("Internal: ${e.fields[0]}") }
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
  const response = harness.net.post(url, payload)
  const parsed = json_parse(response.body)
  parsed
}
```

- Any error in the body triggers a retry. `retry` doesn't inspect the error, so
  a malformed response and an unreachable host are treated the same way.
- If the body succeeds on any attempt, that result is returned immediately.
- `retry N` makes at most N attempts, not N retries after a first try.
- If every attempt fails, the last error propagates. Wrap the block in
  `try`/`catch`, or use a `try` expression, if you want to handle exhaustion
  rather than let it escape.
- `return` inside a retry block propagates out (not retried).

For a retry that reacts to *why* a call failed, use
[`harness.llm.with_rate_limit`](./builtins.md#llm), which backs off only on
`rate_limit`, `overloaded`, `transient_network`, and `timeout`.

## Try-expression

The `try` keyword without a `catch` block acts as a try-expression. It
evaluates the body and returns a `Result`:

- On success: `Result.Ok(value)`
- On error: `Result.Err(error)`

```harn
const result = try { json_parse(raw_input) }
```

This is useful when you want to capture an error as a value rather than
crashing or needing a full `try`/`catch`:

```harn
const parsed = try { json_parse(input) }
if is_err(parsed) {
  harness.stdio.log("Bad input, using defaults")
  parsed = Ok({})
}
const data = unwrap(parsed)
```

## Try/catch expression

`try { ... } catch (e) { ... }` is also usable as an expression — the whole
form evaluates to the try body's tail value on success, or the catch
handler's tail value on a caught throw. The lub of the two branch types is
inferred automatically, and an explicit type annotation on the `let` binds
the result:

```harn,ignore
const parsed: dict = try {
  json_parse(input)
} catch (e) { default_config() }
```

Typed catches work identically in expression position; when the thrown
error's type does not match the catch's type filter, the throw propagates
past the expression and the `let` binding is never established:

```harn,ignore
const user: User = try {
  fetch_user(id)
} catch (e: NetworkError) {
  cached_user(id)
}
// Any non-`NetworkError` throw surfaces out of this block unchanged.
```

A `finally { ... }` tail is optional on either form and runs once for
side-effect only — its value is discarded. The expression's value still
comes from the try body or the catch handler.

The try-expression pairs naturally with the `?` operator. Use `try` to
enter Result-land and `?` to propagate within it:

```harn
fn fetch_json(url) {
  const body = try { harness.net.get(url) }
  const text = unwrap(body)?
  const data = try { json_parse(text) }
  return data
}
```

When `catch` or `finally` follows `try`, the form is the handled
expression described above; only the bare `try { body }` form wraps in
`Result`. If the bare `try` body already returns a `Result`, that result is
returned unchanged instead of being nested as `Result.Ok(Result.Ok(...))`.

## Runtime shape validation errors

When a function parameter has a structural type annotation (a shape like
`{name: string, age: int}`), Harn validates the argument at runtime. If
the argument is missing a required field or a field has the wrong type,
a clear error is produced:

```harn,ignore
fn process(user: {name: string, age: int}) {
  harness.stdio.log("${user.name} is ${user.age}")
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
`Ok(value)` or `Err(error)`. Statically, `Result` is generic:
`Result<T, E>`.

```harn
const ok = Ok(42)
const err = Err("something failed")

const typed_ok: Result<int, string> = ok
const typed_err: Result<int, string> = err

harness.stdio.log(ok)   // Result.Ok(42)
harness.stdio.log(err)  // Result.Err(something failed)
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
const r = Ok(42)
harness.stdio.log(is_ok(r))           // true
harness.stdio.log(is_err(r))          // false
harness.stdio.log(unwrap(r))          // 42
harness.stdio.log(unwrap_or(Err("x"), "default"))  // default
```

### Pattern matching on result

Result values can be destructured with `match`:

```harn
fn fetch_data(url) {
  // ... returns Ok(data) or Err(message)
}

match fetch_data("/api/users") {
  Result.Ok(data) -> { harness.stdio.log("Got ${len(data)} users") }
  Result.Err(err) -> { harness.stdio.log("Failed: ${err}") }
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
  const result = divide(x, 2)?   // unwraps Ok, or returns Err early
  return Ok(result + 10)
}

const r1 = compute(20)  // Result.Ok(20)
const r2 = compute(0)   // Result.Err(division by zero)
```

The `?` operator has the same precedence as `.`, `[]`, and `()`, so it
chains naturally:

```harn
fn fetch_and_parse(url) {
  const response = harness.net.get(url)?
  const data = json_parse(response)?
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
fn transform(data) { return data }

fn parse_json_result(input) {
  const parsed = try {
    json_parse(input)
  }
  if is_err(parsed) {
    return Err("parse error: ${unwrap_err(parsed).message}")
  }
  return parsed
}

fn process(raw) {
  const data = parse_json_result(raw)?   // propagate Err if parse fails
  return Ok(transform(data))
}
```

## Decode at the boundary

Data that enters a script untyped (command output, an HTTP body, a GraphQL
response, a file someone else wrote) should be decoded once, where it enters,
into a declared `type`. Everything after that reads typed fields with `.`.

The alternative hedges every read. This is a Smart Ship helper that finds the
failed merge-queue run for a pull request:

```harn,ignore
const rows = parse_json_or_nil(to_string(child?.stdout ?? ""))
if !command_ok(child) || type_of(rows) != "list" {
  return ""
}
for row in rows {
  const branch = to_string(row?.headBranch ?? "")
  if branch.starts_with(prefix) && row?.conclusion == "failure" {
    return to_string(row?.url ?? "")
  }
}
return ""
```

Nothing declared what a row looks like, so every field is wrapped in `?.`,
`??`, and `to_string`. A renamed field reads as `""` and the helper reports no
failed run instead of an error.

Declare the shape and decode it:

```harn
import { command_run } from "std/command"

type WorkflowRun = {headBranch: string, conclusion: string?, url: string}
type WorkflowRuns = list<WorkflowRun>

fn failed_queue_run(
  tools: HarnessTools,
  repo: string,
  prefix: string,
) -> Result<string?, string> {
  const fields = "headBranch,conclusion,url"
  const child = command_run(
    tools,
    ["gh", "run", "list", "-R", repo, "--json", fields],
  )
  if !child.success {
    return Err(child.stderr)
  }
  match json_decode(child.stdout, schema_of(WorkflowRuns)) {
    Result.Err(error) -> {
      return Err(error.message)
    }
    Result.Ok(runs) -> {
      for run in runs {
        const failed = run.conclusion == "failure"
        if failed && run.headBranch.starts_with(prefix) {
          return Ok(run.url)
        }
      }
    }
  }
  return Ok(nil)
}
```

The pieces:

- `schema_of(T)` turns a `type` alias into its schema at compile time.
- `json_decode(text, schema_of(T))` parses and validates in one step and
  returns `Result<T, SchemaError>`. Malformed JSON and a shape mismatch are
  both `Err`; neither throws.
- `schema_parse(value, schema_of(T))` does the same for a value you already
  have, such as a `dict` from another API. `schema_expect` throws instead of
  returning a `Result`.
- The error record has `message` (the first issue), `errors` (every issue as
  text), and `issues` (`{path, message, code}` for each).
- `match` on the `Result` or postfix `?` keeps `T`; in a function whose error
  type matches, `const runs = json_decode(text, schema_of(WorkflowRuns))?`
  unwraps the `Ok` or returns the `Err`. `unwrap` and `unwrap_err` return a
  dynamic value, so reach for them only when the type does not matter.

How decoding treats fields:

- Unknown fields pass, so a type needs only the fields the script reads.
- `name: T?` must be present but may be `null`. GraphQL returns nullable
  fields this way.
- `name?: T` may also be absent.
- Nested records, lists, and unions of records decode in one call.

Typed results need no decoding. `command_run` returns `CommandResult`,
`harness.process.run`, `exec`, and `shell` return the same record (`stdout`,
`stderr`, `combined`, `exit_code`, `success`, `status`, `timed_out`, ...),
and every buffered `harness.net` request returns `{status, headers, body,
final_url, ok}`. The output, exit, and status fields are never `nil`, and the
checker reports `?.` on them as unnecessary (`HARN-LNT-051`). Only the `body` text is untrusted.

The checker reports a `?.` chain over an untyped value as `HARN-LNT-080`
(`untyped-optional-chain`). It is advisory: it appears in `harn check`,
`harn lint`, and the editor, but does not fail `--strict`. Declaring the type
and decoding once resolves it.

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
    const result = harness.llm.call(prompt, system)
    const parsed = json_parse(result.text)
    return parsed
  } catch (e) {
    harness.stdio.log("Attempt failed: ${e}")
    throw e  // re-throw to trigger retry
  }
}
```
