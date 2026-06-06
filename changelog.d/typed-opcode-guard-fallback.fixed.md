- Typed fast-path opcodes (`AddInt`, `LessInt`, `EqualString`, …) now guard
  their operands and fall back to generic semantics on a type miss instead of
  hard-erroring. The compiler emits these from a static type guess, but a guess
  can be wrong at runtime — an `any`-typed value flowing through a typed
  parameter or an annotated binding initializer (`let x: int = <any>`) is not
  runtime-checked, so the operand may be a different primitive than the
  annotation claims. The optimized build previously threw a
  specialization-internal error (e.g. `Typed int add expected int operands, got
  int and float`) on programs the unoptimized build runs correctly; it now
  produces the same result as the unoptimized build by construction. The hot
  path where the guess holds is unchanged, and genuinely incompatible operands
  still error with the same generic message in both builds.
