- Fixed unsound typed-opcode specialization for reassignable bindings. The
  compiler trusted a `var` / `for`-item binding's initializer-inferred primitive
  type when emitting typed fast-path opcodes (`AddInt`, `LessInt`, …), even
  though such a binding can be reassigned through an `any`-typed value of a
  different runtime primitive. Because typed opcodes hard-error on an operand
  type mismatch, the optimized build could throw a spurious
  `Typed int add expected int operands, got int and float` on a program the
  unoptimized build runs correctly. The compiler now keeps the typed fast path
  only for bindings a new monomorphism analysis can prove keep a single
  primitive type across their initializer and every reassignment in scope; all
  others fall back to the generic adaptive path, which re-checks operand shapes
  at runtime. The common loop-counter and accumulator idioms stay fully
  specialized.
