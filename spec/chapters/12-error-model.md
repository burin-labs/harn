## Error model

### throw

```harn
throw expression
```

Evaluates the expression and throws it as `HarnRuntimeError.thrownError(value)`.
Any value can be thrown (strings, dicts, etc.).

### try/catch/finally

```harn
try {
  // body
} catch (e) {
  // handler
} finally {
  // cleanup — always runs
}
```

If the body throws:

- A `thrownError(value)`: `e` is bound to the thrown value directly.
- Any other runtime error: `e` is bound to the error's `localizedDescription` string.

`return` inside a `try` block propagates out of the enclosing pipeline (is not caught).

The error variable `(e)` is optional: `catch { ... }` is valid without it.

`try { ... } catch (e) { ... }` is also usable as an expression: the value of
the whole form is the tail value of the try body when it succeeds, and the tail
value of the catch handler when an error is caught. This means the natural
`let v = try { risky() } catch (e) { fallback }` binding is supported directly,
without needing to restructure through `Result` helpers. When a typed catch
(`catch (e: AppError) { ... }`) does not match the thrown error's type, the
throw propagates past the expression unchanged — the surrounding `let` never
binds. See the [Try-expression](#try-expression) section below for the
`Result`-wrapping behavior when `catch` is omitted.

### try* (rethrow-into-catch)

`try* EXPR` is a prefix operator that evaluates `EXPR` and rethrows any
thrown error so an enclosing `try { ... } catch (e) { ... }` can handle
it, instead of forcing the caller to manually convert thrown errors
into a `Result` and then `guard is_ok / unwrap`. The lowered form is:

```harn,ignore
{ const _r = try { EXPR }
  guard is_ok(_r) else { throw unwrap_err(_r) }
  unwrap(_r) }
```

On success `try* EXPR` evaluates to `EXPR`'s value with no `Result`
wrapping. The rethrow runs every `finally` block between the rethrow
site and the innermost catch handler exactly once, matching the
`finally` exactly-once guarantee for plain `throw`.

```harn,ignore
fn fetch(prompt) {
  // Without try*: try { llm_call(prompt) } / guard is_ok / unwrap
  const response = try* llm_call(prompt)
  return parse(response)
}

const outcome = try {
  const result = fetch(prompt)
  Ok(result)
} catch (e: ApiError) {
  Err(e.code)
}
```

`try*` requires an enclosing function (`fn`, `tool`, or `pipeline`) so
the rethrow has a body to live in — using it at module top level is a
compile error. The operand is parsed at unary-prefix precedence, so
`try* foo.bar(1)` parses as `try* (foo.bar(1))` and `try* a + b` parses
as `(try* a) + b`. Use parentheses to combine `try*` with binary
operators on its operand. `try*` is distinct from the postfix `?`
operator: `?` early-returns `Result.Err(...)` from a `Result`-returning
function, while `try*` rethrows a thrown value into an enclosing catch.

### finally

The `finally` block is optional and runs regardless of whether the try body
succeeds, throws, or the catch body re-throws. Supported forms:

```harn,ignore
try { ... } catch e { ... } finally { ... }
try { ... } finally { ... }
try { ... } catch e { ... }
```

`return`, `break`, and `continue` inside a try body with a finally block will
execute the finally block before the control flow transfer completes.

The finally block's return value is discarded — the overall expression value
comes from the try or catch body.

