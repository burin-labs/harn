# HARN-MET-001 — expression is not permitted in a const initializer

**Category:** Meta (MET)
**Variant:** `Code::ConstEvalDisallowedExpression`

## What it means

A `const` binding's right-hand side must be a pure expression that can be
folded at compile time by the bounded const-evaluator. The expression
referenced something the evaluator does not permit: a call into a non-const
function, a property access on a host object such as `harness`, a runtime
construct (spawn / parallel / select / try / yield / emit / await), a
loop, an assignment, or any builtin outside the curated const-friendly
allowlist.

Rejected:

```harn,ignore
const Z = harness.clock.now()              // host capability
const W = spawn { 1 }                      // runtime construct
const Q = some_user_fn()                   // user function call
```

Accepted:

```harn
const X: int = 5 + 3
const Y: string = format("hello-{}", X)
const NS: list = [1, 2, X]
const COUNT: int = len([1, 2, 3])

// reads to silence the unused-variable lint in the example
let _ = [X, Y, NS, COUNT]
```

## How to fix

- Move the side-effecting computation into a regular `let` binding inside a
  pipeline or function body.
- If the value really is a compile-time constant, restructure it as
  arithmetic, string concatenation, literal collections, or reads of
  earlier `const` bindings.

## Stability

This code is stable. Adding additional pure builtins to the const-eval
allowlist remains backwards-compatible — newly permitted expressions
simply stop being rejected.
