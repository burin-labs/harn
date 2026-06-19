- **`assert(cond)` and `require cond` now narrow types like a guard.** Both
  diverge (throw) when `cond` is falsy, so code after them may rely on the
  truthy refinement: `assert(x != nil, ...)` (or `require x != nil`) followed
  by `x + 1` now type-checks without a `??`, matching how an `if x == nil`
  guard already narrows. This is the TypeScript "assertion function" model and
  removes friction for the idiomatic `assert(value != nil)` test/precondition
  pattern.
