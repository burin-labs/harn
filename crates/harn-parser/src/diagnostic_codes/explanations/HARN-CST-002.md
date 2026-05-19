# HARN-CST-002 — const initializer exceeded the recursion depth budget

**Category:** Const-eval sandbox (CST)
**Variant:** `Code::ConstEvalRecursionLimit`

## What it means

The compile-time evaluator tracks how deeply it has recursed into nested
expressions or conditionals. Exceeding `MAX_DEPTH` (default 256) aborts
evaluation with this diagnostic. The cap protects the compiler thread's
stack and prevents pathological deeply-nested literals from creating an
unbounded stack frame chain.

## How to fix

- Flatten deeply-nested literals (e.g. a 1,000-level nested ternary).
- Break the constant into intermediate `const` bindings.

## Stability

This code is stable. The default budget may grow over time; tightening it
would require a deprecation cycle.
