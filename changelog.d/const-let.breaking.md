- **Variable-binding keywords now follow the TypeScript/Swift convention.**
  `const` is the immutable default binding (formerly `let`) and `let` is the
  mutable binding (formerly `var`); `var` is removed and now produces a
  migration diagnostic. `const` is a normal immutable binding that accepts any
  initializer — the old strict compile-time-constant rejection is gone (pure
  initializers are still folded as a transparent optimization). Every `.harn`
  source must be migrated; see `docs/src/migrations/const-let.md` for the
  automated `harn codemod` rules.
