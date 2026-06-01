- **Un-annotated function return types are now inferred from the body**, so
  calling a helper without a `-> T` annotation recovers a precise type instead
  of going untyped: `fn area(w: int, h: int) { w * h }` now returns `int` at its
  call sites. Inference is sound by construction — it assigns a return type only
  when *every* return path (and any implicit fall-through value) is concretely
  known, otherwise the function stays dynamic. Recursion is self-guarding: a
  self/mutual/forward call resolves to the hoisted placeholder signature, so the
  function simply stays dynamic rather than looping. Inferred return types drive
  call-site inference only; they never trigger the declared-return diagnostics
  (fall-through / mismatch), which remain reserved for explicit annotations.
- **`+`/`-`/`*`/`/` no longer report a spurious "can't …" error when an operand
  has a gradual static type** (`any` / `unknown` / `_`). A gradual operand is
  compatible with every operator and the real check is deferred to runtime,
  matching how untyped operands were already treated. The gradual-top-type set
  is now centralized in one `is_gradual_type_name` predicate.
- **A local binding shadows a same-named function even when its static type is
  unknown.** `var x = …` / `let x = …` that reuses a function's name now
  resolves to the local, not the function reference, fixing a case where a
  shadowing local with an unknown type was mis-typed as `fn(…) -> …`.
