# HARN-CST-004 — const initializer raised a runtime error during evaluation

## What it means

The expression was syntactically eligible for compile-time evaluation,
but the evaluator hit a value-level error during folding: integer
overflow on a literal arithmetic, division by zero, indexing past the
end of a literal list, an undefined identifier the const-eval
environment cannot resolve, or a type mismatch on a binary operator.

```harn,ignore
// Rejected at compile time:
const ZERO = 1 / 0
const OOB = [1, 2, 3][9]
const BAD = "a" + 1
```

## How to fix

- Inspect the offending operand and supply a valid value.
- If the expression depends on a value that is only known at runtime,
  use `let` instead of `const`.
