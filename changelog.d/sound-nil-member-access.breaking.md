- **Member-access nil safety is now uniform across `.`, `[]`, and `.()`.**
  Subscript (`obj[key]`) and method-call (`obj.method(..)`) receivers are
  now held to the same standard the checker already applied to property
  reads: a statically-`nil` or `T | nil` receiver is an **error**, and an
  `unknown` receiver is a **warning**. Previously only `obj.field` was
  diagnosed, so `obj[key]` / `obj.method()` on a possibly-nil value passed
  `harn check` and failed at runtime instead. Migrate with the matching
  optional operator (`?[…]`, `?.method()`), a `!= nil` guard, or a `??`
  default. `any` receivers remain a deliberate, undiagnosed escape hatch,
  and the ambient dict-literal idiom (`let d = {a: 1}; d["b"]`) stays loose.

  To keep the stricter rule pleasant, two long-standing narrowing gaps were
  closed alongside it: an `o?.field != nil` (or `?[]` / `?.()`) guard now
  narrows the **base** identifier `o` to non-nil on the matching branch, and
  the `??` coalesce operator now drops the nil arm even when its left operand
  is a **named type alias** that expands to a nilable union (previously only
  inline `T | nil` unions were narrowed). The conformance harness also labels
  failures by stage — `type error` / `compile error` / `runtime error` —
  instead of calling every pre-runtime failure a "runtime error".
