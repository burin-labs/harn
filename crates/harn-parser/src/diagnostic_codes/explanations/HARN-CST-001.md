# HARN-CST-001 — const initializer exceeded the step budget

**Category:** Const-eval sandbox (CST)
**Variant:** `Code::ConstEvalStepLimit`

## What it means

The bounded compile-time evaluator counts every reduction step it
performs while folding a `const` initializer. When the running count
exceeds `MAX_STEPS` (default 100,000), evaluation is aborted with this
diagnostic. The cap is enforced on every step, not amortized, so a
hostile or accidental quadratic expression cannot stall the compiler.

```harn,ignore
// Rejected (would expand far beyond the step budget):
const HUGE = sum_to(1_000_000)
```

## How to fix

- Pre-compute the value off-line and embed a literal.
- Reduce the expression to a smaller closed form.
- If the work genuinely belongs at runtime, switch from `const` to `let`.

## Stability

This code is stable. The default budget may grow over time; tightening it
would require a deprecation cycle.
