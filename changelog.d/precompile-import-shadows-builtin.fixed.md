- **`harn precompile` now resolves imports before type-checking, so an
  imported symbol that shares a name with a builtin no longer reports phantom
  type errors.** Calling `render` imported from `std/disclosure` (or any
  stdlib/user export colliding with a builtin such as the `render` template
  helper) compiled and ran fine under `harn run` but failed `harn precompile`,
  because the precompiler checked the call against the builtin's signature
  instead of the import. Precompile now derives the import graph like
  `harn run`/`harn check`, and an imported name shadows a same-named builtin in
  the type checker.
